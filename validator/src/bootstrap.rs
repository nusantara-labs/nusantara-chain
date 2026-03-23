use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use borsh::BorshDeserialize;
use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::commitment::CommitmentTracker;
use nusantara_consensus::fork_choice::ForkTree;
use nusantara_consensus::gpu::GpuPohVerifier;
use nusantara_consensus::leader_schedule::LeaderScheduleGenerator;
use nusantara_consensus::replay_stage::ReplayStage;
use nusantara_consensus::tower::Tower;
use nusantara_core::{DEFAULT_SLOT_DURATION_MS, FeeCalculator};
use nusantara_crypto::{Hash, Keypair};
use nusantara_genesis::{
    GENESIS_HASH_KEY, GenesisBuilder, GenesisConfig, GenesisValidatorInfo, VALIDATORS_KEY,
};
use nusantara_gossip::{ClusterInfo, ContactInfo};
use nusantara_mempool::Mempool;
use nusantara_rpc::RpcState;
use nusantara_runtime::ProgramCache;
use nusantara_stake_program::Delegation;
use nusantara_storage::Storage;
use nusantara_storage::cf::CF_DEFAULT;
use nusantara_sysvar_program::{Clock, EpochScheduleSysvar, RentSysvar};
use nusantara_turbine::ShredCollector;
use nusantara_vote_program::{VoteInit, VoteState};
use tracing::{info, warn};

use crate::block_producer::BlockProducer;
use crate::cli::Cli;
use crate::constants::SharedLeaderCache;
use crate::error::ValidatorError;
use crate::node::ValidatorNode;
use crate::slot_clock::SlotClock;

impl ValidatorNode {
    pub fn boot(cli: &Cli) -> Result<Self, ValidatorError> {
        // 1. Open storage
        let storage_path = Path::new(&cli.ledger_path);
        let storage = Arc::new(Storage::open(storage_path)?);
        info!(path = %cli.ledger_path, "storage opened");

        // 2. Load or generate identity keypair
        let keypair = Arc::new(crate::identity::load_or_generate_keypair(cli)?);
        let identity_address = keypair.address();
        info!(identity = %identity_address.to_base64(), "identity loaded");

        // 2b. Check for and clean up a partial snapshot restore from a prior crash
        if nusantara_storage::snapshot_archive::cleanup_partial_snapshot_restore(&storage)? {
            warn!("detected partial snapshot restore from previous crash — cleaned up marker");
        }

        // 2c. Attempt snapshot restore before genesis
        let snapshot_dir = Path::new(&cli.ledger_path).join("snapshots");
        if storage.get_cf(CF_DEFAULT, GENESIS_HASH_KEY)?.is_none()
            && let Some(snapshot_path) =
                nusantara_storage::snapshot_archive::find_latest_snapshot_file(&snapshot_dir)
        {
            info!(
                path = %snapshot_path.display(),
                "found snapshot file, restoring state"
            );
            let archive = nusantara_storage::snapshot_archive::load_from_file(&snapshot_path)?;
            nusantara_storage::snapshot_archive::restore_snapshot(&storage, &archive)?;
            info!(
                slot = archive.manifest.slot,
                accounts = archive.manifest.account_count,
                "state restored from snapshot"
            );
        }

        // 3. Ensure genesis is applied
        if storage.get_cf(CF_DEFAULT, GENESIS_HASH_KEY)?.is_none() {
            let genesis_path = cli
                .genesis_config
                .as_deref()
                .ok_or(ValidatorError::NoGenesis)?;
            info!(path = genesis_path, "applying genesis config");
            let mut config = GenesisConfig::load(genesis_path)?;

            // Bind genesis validators to keypairs
            let mut extra_idx = 0;
            for (i, validator) in config.validators.iter_mut().enumerate() {
                if validator.identity == "generate" {
                    if i == 0 {
                        validator.identity = identity_address.to_base64();
                        info!("bound genesis validator[0] identity to this node's keypair");
                    } else if extra_idx < cli.extra_validator_keys.len() {
                        let extra_kp =
                            crate::identity::load_keypair_from_path(&cli.extra_validator_keys[extra_idx])?;
                        validator.identity = extra_kp.address().to_base64();
                        info!(
                            validator_index = i,
                            path = %cli.extra_validator_keys[extra_idx],
                            "bound genesis validator identity from extra keypair"
                        );
                        extra_idx += 1;
                    } else {
                        let auto_kp = Keypair::generate();
                        validator.identity = auto_kp.address().to_base64();
                        info!(
                            validator_index = i,
                            "auto-generated keypair for genesis validator"
                        );
                    }
                }
            }

            let builder = GenesisBuilder::new(&config, &storage);
            let result = builder.build()?;
            info!(
                genesis_hash = %result.genesis_hash.to_base64(),
                validators = result.validator_count,
                total_stake = result.total_stake,
                total_supply = result.total_supply,
                "genesis applied"
            );
        } else {
            info!("existing genesis found in storage");
        }

        // 4. Load sysvars from storage
        let clock: Clock = storage
            .get_sysvar::<Clock>()?
            .ok_or(ValidatorError::NoGenesis)?;
        let rent_sysvar: RentSysvar = storage
            .get_sysvar::<RentSysvar>()?
            .ok_or(ValidatorError::NoGenesis)?;
        let epoch_sysvar: EpochScheduleSysvar = storage
            .get_sysvar::<EpochScheduleSysvar>()?
            .ok_or(ValidatorError::NoGenesis)?;

        let epoch_schedule = epoch_sysvar.0;
        let rent = rent_sysvar.0;
        let fee_calculator = FeeCalculator::default();

        // 5. Determine last root slot and hashes
        let last_root = storage.get_latest_root()?.unwrap_or(0);
        let genesis_hash_bytes = storage
            .get_cf(CF_DEFAULT, GENESIS_HASH_KEY)?
            .ok_or(ValidatorError::NoGenesis)?;
        let genesis_hash = Hash::new(
            genesis_hash_bytes
                .try_into()
                .map_err(|_| ValidatorError::Keypair("invalid genesis hash".to_string()))?,
        );

        let parent_hash = storage.get_slot_hash(last_root)?.unwrap_or(genesis_hash);
        let parent_bank_hash = storage
            .get_bank_hash(last_root)?
            .unwrap_or_else(|| crate::identity::hashv_bank_genesis(&genesis_hash));

        info!(
            last_root,
            parent_hash = %parent_hash.to_base64(),
            "loaded chain state"
        );

        // 6. Create ConsensusBank
        let bank = Arc::new(ConsensusBank::new(
            Arc::clone(&storage),
            epoch_schedule.clone(),
        ));

        bank.advance_slot(last_root, clock.unix_timestamp);
        bank.record_slot_hash(0, genesis_hash);
        if last_root > 0 {
            bank.record_slot_hash(last_root, parent_hash);
        }

        // 7-8. Load genesis validators and register in bank
        let mut validators: Vec<GenesisValidatorInfo> = Vec::new();
        if let Some(validators_data) = storage.get_cf(CF_DEFAULT, VALIDATORS_KEY)? {
            validators = BorshDeserialize::deserialize(&mut validators_data.as_slice())
                .map_err(|e| ValidatorError::Keypair(format!("failed to load validators: {e}")))?;

            for v in &validators {
                if let Some(vote_account) = storage.get_account(&v.vote_account)? {
                    let vote_state: VoteState = BorshDeserialize::deserialize(
                        &mut vote_account.data.as_slice(),
                    )
                    .map_err(|e| {
                        ValidatorError::Keypair(format!("failed to deserialize vote state: {e}"))
                    })?;
                    bank.set_vote_state(v.vote_account, vote_state);
                }

                let delegation = Delegation {
                    voter_pubkey: v.vote_account,
                    stake: v.stake_lamports,
                    activation_epoch: 0,
                    deactivation_epoch: u64::MAX,
                    warmup_cooldown_rate_bps:
                        nusantara_stake_program::DEFAULT_WARMUP_COOLDOWN_RATE_BPS,
                };
                bank.set_stake_delegation(v.stake_account, delegation);
            }

            info!(
                count = validators.len(),
                "loaded genesis validators into bank"
            );
        } else {
            warn!("no validator info found in storage");
        }

        // 9. Recalculate epoch stakes
        let current_epoch = epoch_schedule.get_epoch(last_root);
        bank.recalculate_epoch_stakes(current_epoch);
        info!(
            epoch = current_epoch,
            total_stake = bank.total_active_stake(),
            "epoch stakes calculated"
        );

        // 9b. Initialize state Merkle tree from all accounts in storage
        let state_tree = nusantara_consensus::StateTree::init_from_storage(&storage)?;
        info!(
            leaves = state_tree.len(),
            "state tree initialized from storage"
        );
        bank.set_state_tree(state_tree);

        // 10. Create SlotClock
        let slot_clock = SlotClock::new(clock.epoch_start_timestamp, DEFAULT_SLOT_DURATION_MS);
        let current_slot = slot_clock.current_slot().max(last_root + 1);

        // 11. Create ProgramCache
        let program_cache = Arc::new(ProgramCache::new(crate::constants::PROGRAM_CACHE_SIZE));

        // 12. Create BlockProducer
        let hashes_per_tick = cli
            .hashes_per_tick
            .unwrap_or(nusantara_consensus::poh::HASHES_PER_TICK);
        let block_producer = BlockProducer::new(
            identity_address,
            Arc::clone(&storage),
            Arc::clone(&bank),
            parent_hash,
            epoch_schedule.clone(),
            fee_calculator.clone(),
            rent.clone(),
            last_root,
            parent_hash,
            parent_bank_hash,
            Arc::clone(&program_cache),
            hashes_per_tick,
        );

        // 12. Create mempool
        let mempool = Arc::new(Mempool::new(
            nusantara_mempool::pool::DEFAULT_MAX_SIZE as usize,
        ));

        // 13. Parse network addresses
        let gossip_addr: SocketAddr = cli
            .gossip_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid gossip addr: {e}")))?;
        let turbine_addr: SocketAddr = cli
            .turbine_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid turbine addr: {e}")))?;
        let repair_addr: SocketAddr = cli
            .repair_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid repair addr: {e}")))?;
        let tpu_addr: SocketAddr = cli
            .tpu_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid tpu addr: {e}")))?;
        let tpu_forward_addr: SocketAddr = cli
            .tpu_forward_addr
            .parse()
            .map_err(|e| ValidatorError::NetworkInit(format!("invalid tpu forward addr: {e}")))?;

        // 14. Build ContactInfo
        let (adv_gossip, adv_tpu, adv_tpu_fwd, adv_turbine, adv_repair) =
            if let Some(ref host) = cli.public_host {
                let public_ip = crate::identity::resolve_public_host(host)?;
                info!(host, ip = %public_ip, "resolved public host");
                (
                    SocketAddr::new(public_ip, gossip_addr.port()),
                    SocketAddr::new(public_ip, tpu_addr.port()),
                    SocketAddr::new(public_ip, tpu_forward_addr.port()),
                    SocketAddr::new(public_ip, turbine_addr.port()),
                    SocketAddr::new(public_ip, repair_addr.port()),
                )
            } else {
                (
                    gossip_addr,
                    tpu_addr,
                    tpu_forward_addr,
                    turbine_addr,
                    repair_addr,
                )
            };

        let wallclock = crate::helpers::unix_timestamp_millis();

        let contact_info = ContactInfo::new(
            keypair.public_key().clone(),
            adv_gossip,
            adv_tpu,
            adv_tpu_fwd,
            adv_turbine,
            adv_repair,
            cli.shred_version,
            wallclock,
        );

        // 15. Create ClusterInfo
        let entrypoints: Vec<SocketAddr> = cli
            .entrypoints
            .iter()
            .filter_map(|ep| {
                if let Ok(addr) = ep.parse() {
                    return Some(addr);
                }
                // Blocking DNS is acceptable here — runs once at startup before async loop.
                match std::net::ToSocketAddrs::to_socket_addrs(&ep.as_str()) {
                    Ok(mut addrs) => {
                        if let Some(addr) = addrs.next() {
                            info!(entrypoint = ep, resolved = %addr, "resolved entrypoint hostname");
                            Some(addr)
                        } else {
                            warn!(entrypoint = ep, "hostname resolved to no addresses, skipping");
                            None
                        }
                    }
                    Err(e) => {
                        warn!(entrypoint = ep, error = %e, "failed to resolve entrypoint, skipping");
                        None
                    }
                }
            })
            .collect();

        let cluster_info = Arc::new(ClusterInfo::new(
            Arc::clone(&keypair),
            contact_info,
            entrypoints,
            crate::constants::CLUSTER_INFO_TIMEOUT_MS,
        ));

        // 16. Build ReplayStage
        let tower = Tower::new(VoteState::new(&VoteInit {
            node_pubkey: identity_address,
            authorized_voter: identity_address,
            authorized_withdrawer: identity_address,
            commission: 0,
        }));
        let fork_tree = ForkTree::new(last_root, parent_hash, parent_bank_hash);
        let commitment_tracker = CommitmentTracker::new(bank.total_active_stake());
        let gpu_verifier = GpuPohVerifier::new().ok().flatten();
        let mut replay_stage = ReplayStage::new(
            identity_address,
            Arc::clone(&bank),
            tower,
            fork_tree,
            commitment_tracker,
            gpu_verifier,
        );

        // 17. Compute initial leader schedule
        let leader_cache: SharedLeaderCache = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let leader_schedule_generator = LeaderScheduleGenerator::new(epoch_schedule.clone());

        let stakes = bank.get_stake_distribution();
        if let Ok(schedule) =
            leader_schedule_generator.compute_schedule(current_epoch, &stakes, &genesis_hash)
        {
            replay_stage.cache_leader_schedule(current_epoch, schedule.clone());
            leader_cache.write().insert(current_epoch, schedule);
            info!(epoch = current_epoch, "initial leader schedule computed");
        }

        // 18. Look up own vote account from genesis validators
        let my_vote_account = validators
            .iter()
            .find(|v| v.identity == identity_address)
            .map(|v| v.vote_account);

        if let Some(va) = my_vote_account {
            info!(vote_account = %va.to_base64(), "found own vote account");
        } else {
            warn!("no vote account found for this identity — votes will not be submitted");
        }

        info!(
            start_slot = current_slot,
            identity = %identity_address.to_base64(),
            gossip = %gossip_addr,
            turbine = %turbine_addr,
            tpu = %tpu_addr,
            peers = cluster_info.entrypoints().len(),
            "validator ready"
        );

        let gossip_vote_cursor = cluster_info.crds().current_cursor();

        Ok(Self {
            keypair,
            identity: identity_address,
            storage,
            bank,
            block_producer,
            mempool,
            slot_clock,
            current_slot,
            cluster_info,
            replay_stage,
            leader_cache,
            leader_schedule_generator,
            epoch_schedule,
            genesis_hash,
            my_vote_account,
            gossip_addr,
            turbine_addr,
            repair_addr,
            tpu_addr,
            tpu_forward_addr,
            consecutive_skips: Arc::new(AtomicU64::new(0)),
            total_skips: 0,
            gossip_vote_cursor,
            slash_detector: nusantara_consensus::SlashDetector::new(),
            fee_calculator,
            rent,
            program_cache,
            pubsub_tx: RpcState::new_pubsub_channel(),
            orphan_blocks: BTreeMap::new(),
            shred_collector: Arc::new(ShredCollector::new()),
            snapshot_dir: Path::new(&cli.ledger_path).join("snapshots"),
            failed_fork_targets: HashSet::new(),
            last_voted_slot: current_slot,
            last_produced_parent: None,
            max_txs_per_slot: cli.max_txs_per_slot,
        })
    }
}
