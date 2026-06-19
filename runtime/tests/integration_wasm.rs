use nusantara_core::program::LOADER_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hashv};
use nusantara_loader_program::state::LoaderState;
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_transaction};
use nusantara_storage::Storage;
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};
use tempfile::tempdir;

fn test_sysvars() -> SysvarCache {
    SysvarCache::new(
        Clock::default(),
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![Hash::zero()]),
    )
}

fn test_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

/// Minimal valid WASM module that exports `entrypoint` and `memory`.
/// Equivalent WAT:
/// ```wat
/// (module
///   (memory (export "memory") 1)
///   (func (export "entrypoint") (param i32 i32 i32 i32) (result i64)
///     i64.const 0))
/// ```
fn minimal_wasm() -> Vec<u8> {
    // Precompiled from the WAT above — hand-assembled with correct section sizes
    vec![
        0x00, 0x61, 0x73, 0x6d, // magic
        0x01, 0x00, 0x00, 0x00, // version
        // Type section (id=1): 1 func type (i32,i32,i32,i32)->i64
        0x01, 0x09, 0x01, // section id=1, size=9, count=1
        0x60, // func type
        0x04, 0x7f, 0x7f, 0x7f, 0x7f, // 4 params: i32 i32 i32 i32
        0x01, 0x7e, // 1 result: i64
        // Function section (id=3): 1 function using type 0
        0x03, 0x02, 0x01, // section id=3, size=2, count=1
        0x00, // type index 0
        // Memory section (id=5): 1 memory, min=49 pages, max=64 pages.
        // min=49 ensures HEAP_START (page 48) + program_id (64 B) is in-bounds.
        0x05, 0x04, 0x01, // section id=5, size=4, count=1
        0x01, 0x31, 0x40, // limits: flag=has_max, min=49, max=64
        // Export section (id=7): 2 exports
        0x07, 0x17, 0x02, // section id=7, size=23, count=2
        // export "memory" (memory 0)
        0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00,
        // export "entrypoint" (func 0)
        0x0a, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x70, 0x6f, 0x69, 0x6e, 0x74, 0x00, 0x00,
        // Code section (id=10): 1 function body
        0x0a, 0x06, 0x01, // section id=10, size=6, count=1
        0x04, // body size=4
        0x00, // local decl count=0
        0x42, 0x00, // i64.const 0
        0x0b, // end
    ]
}

/// A second valid WASM module that returns 0 (for upgrade testing).
/// Same structure but produces a different bytecode hash (extra unreachable path).
fn minimal_wasm_v2() -> Vec<u8> {
    // Same module but with a nop before the return (different bytecode hash)
    vec![
        0x00, 0x61, 0x73, 0x6d, // magic
        0x01, 0x00, 0x00, 0x00, // version
        // Type section
        0x01, 0x09, 0x01, 0x60, 0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x01, 0x7e,
        // Function section
        0x03, 0x02, 0x01, 0x00, // Memory section
        0x05, 0x04, 0x01, 0x01, 0x31, 0x40, // Export section
        0x07, 0x17, 0x02, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x0a, 0x65, 0x6e,
        0x74, 0x72, 0x79, 0x70, 0x6f, 0x69, 0x6e, 0x74, 0x00, 0x00,
        // Code section: body with nop
        0x0a, 0x07, 0x01, 0x05, 0x00, // body size=5, local decl count=0
        0x01, // nop
        0x42, 0x00, // i64.const 0
        0x0b, // end
    ]
}

fn commit_deltas(storage: &Storage, slot: u64, result: &nusantara_runtime::TransactionResult) {
    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, slot, account).unwrap();
    }
}

/// Full deploy pipeline: InitializeBuffer → Write → Deploy → verify
#[test]
fn deploy_and_verify() {
    let (storage, _dir) = test_storage();
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();
    let cache = ProgramCache::new(16);

    let payer_kp = Keypair::generate();
    let payer = payer_kp.address();
    let buffer_kp = Keypair::generate();
    let buffer = buffer_kp.address();
    let program_kp = Keypair::generate();
    let program = program_kp.address();
    let program_data = hashv(&[b"program_data", program.as_bytes()]);

    let wasm = minimal_wasm();
    let rent = Rent::default();
    let pd_header_size = LoaderState::program_data_header_size();
    let pd_total_size = pd_header_size + wasm.len() * 2;
    let pd_rent = rent.minimum_balance(pd_total_size);

    // Fund payer generously
    storage
        .put_account(
            &payer,
            0,
            &Account::new(100_000_000_000, *LOADER_PROGRAM_ID),
        )
        .unwrap();

    // Step 1: InitializeBuffer (buffer=signer, authority/payer=signer)
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer, &payer);
    let msg = Message::new(&[init_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &buffer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);
    assert!(
        result.status.is_ok(),
        "init buffer failed: {:?}",
        result.status
    );
    commit_deltas(&storage, 1, &result);

    // Step 2: Write bytecode (authority/payer=signer)
    let write_ix = nusantara_loader_program::write(&buffer, &payer, 0, wasm.clone());
    let msg = Message::new(&[write_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 2, &cache, None, false);
    assert!(result.status.is_ok(), "write failed: {:?}", result.status);
    commit_deltas(&storage, 2, &result);

    // Step 3: Deploy (payer=signer, program=signer, authority=signer; payer==authority)
    let deploy_ix = nusantara_loader_program::deploy(
        &payer,
        &program,
        &program_data,
        &buffer,
        &payer,
        (wasm.len() * 2) as u64,
    );
    let msg = Message::new(&[deploy_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &program_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 3, &cache, None, false);
    assert!(result.status.is_ok(), "deploy failed: {:?}", result.status);
    commit_deltas(&storage, 3, &result);

    // Verify: program account
    let prog_acc = storage.get_account(&program).unwrap().unwrap();
    assert!(prog_acc.executable);
    assert_eq!(prog_acc.owner, *LOADER_PROGRAM_ID);

    let state = LoaderState::from_account_data(&prog_acc.data).unwrap();
    match state {
        LoaderState::Program {
            program_data_address,
        } => {
            assert_eq!(program_data_address, program_data);
        }
        _ => panic!("expected Program state"),
    }

    // Verify: program data account has bytecode
    let pd_acc = storage.get_account(&program_data).unwrap().unwrap();
    assert!(pd_acc.lamports >= pd_rent);
    let bytecode = LoaderState::extract_bytecode(&pd_acc.data).unwrap();
    assert_eq!(&bytecode[..wasm.len()], wasm.as_slice());

    // Verify: buffer is closed
    let buf_acc = storage.get_account(&buffer).unwrap().unwrap();
    assert_eq!(buf_acc.lamports, 0);
    assert!(buf_acc.data.is_empty());
}

/// Deploy → build instruction targeting program → execute → verify success
#[test]
fn invoke_deployed_program() {
    let (storage, _dir) = test_storage();
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();
    let cache = ProgramCache::new(16);

    let payer_kp = Keypair::generate();
    let payer = payer_kp.address();
    let buffer_kp = Keypair::generate();
    let buffer = buffer_kp.address();
    let program_kp = Keypair::generate();
    let program = program_kp.address();
    let program_data = hashv(&[b"program_data", program.as_bytes()]);
    let wasm = minimal_wasm();

    storage
        .put_account(
            &payer,
            0,
            &Account::new(100_000_000_000, *LOADER_PROGRAM_ID),
        )
        .unwrap();

    // Deploy sequence
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer, &payer);
    let msg = Message::new(&[init_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &buffer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 1, &result);

    let write_ix = nusantara_loader_program::write(&buffer, &payer, 0, wasm.clone());
    let msg = Message::new(&[write_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 2, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 2, &result);

    let deploy_ix = nusantara_loader_program::deploy(
        &payer,
        &program,
        &program_data,
        &buffer,
        &payer,
        (wasm.len() * 2) as u64,
    );
    let msg = Message::new(&[deploy_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &program_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 3, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 3, &result);

    // Now invoke the deployed program
    use nusantara_core::instruction::{AccountMeta, Instruction};
    let invoke_ix = Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(program, false),
            AccountMeta::new_readonly(program_data, false),
        ],
        data: vec![],
    };
    let msg = Message::new(&[invoke_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 4, &cache, None, false);
    assert!(result.status.is_ok(), "invoke failed: {:?}", result.status);
}

/// Deploy v1 → create new buffer with v2 → Upgrade → verify ProgramData updated
#[test]
fn upgrade_program() {
    let (storage, _dir) = test_storage();
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();
    let cache = ProgramCache::new(16);

    let payer_kp = Keypair::generate();
    let payer = payer_kp.address();
    let buffer_v1_kp = Keypair::generate();
    let buffer_v1 = buffer_v1_kp.address();
    let buffer_v2_kp = Keypair::generate();
    let buffer_v2 = buffer_v2_kp.address();
    let program_kp = Keypair::generate();
    let program = program_kp.address();
    let program_data = hashv(&[b"program_data", program.as_bytes()]);
    let wasm_v1 = minimal_wasm();
    let wasm_v2 = minimal_wasm_v2();

    storage
        .put_account(
            &payer,
            0,
            &Account::new(100_000_000_000, *LOADER_PROGRAM_ID),
        )
        .unwrap();

    // Deploy v1
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer_v1, &payer);
    let msg = Message::new(&[init_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &buffer_v1_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 1, &result);

    let write_ix = nusantara_loader_program::write(&buffer_v1, &payer, 0, wasm_v1.clone());
    let msg = Message::new(&[write_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 2, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 2, &result);

    let deploy_ix = nusantara_loader_program::deploy(
        &payer,
        &program,
        &program_data,
        &buffer_v1,
        &payer,
        (wasm_v1.len().max(wasm_v2.len()) * 2) as u64,
    );
    let msg = Message::new(&[deploy_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &program_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 3, &cache, None, false);
    assert!(
        result.status.is_ok(),
        "deploy v1 failed: {:?}",
        result.status
    );
    commit_deltas(&storage, 3, &result);

    // Verify v1 bytecode
    let pd_acc = storage.get_account(&program_data).unwrap().unwrap();
    let bytecode = LoaderState::extract_bytecode(&pd_acc.data).unwrap();
    assert_eq!(&bytecode[..wasm_v1.len()], wasm_v1.as_slice());

    // Create v2 buffer
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer_v2, &payer);
    let msg = Message::new(&[init_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &buffer_v2_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 4, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 4, &result);

    let write_ix = nusantara_loader_program::write(&buffer_v2, &payer, 0, wasm_v2.clone());
    let msg = Message::new(&[write_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 5, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 5, &result);

    // Upgrade (authority=signer)
    let upgrade_ix = nusantara_loader_program::upgrade(&program, &program_data, &buffer_v2, &payer);
    let msg = Message::new(&[upgrade_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 6, &cache, None, false);
    assert!(result.status.is_ok(), "upgrade failed: {:?}", result.status);
    commit_deltas(&storage, 6, &result);

    // Verify v2 bytecode
    let pd_acc = storage.get_account(&program_data).unwrap().unwrap();
    let bytecode = LoaderState::extract_bytecode(&pd_acc.data).unwrap();
    assert_eq!(&bytecode[..wasm_v2.len()], wasm_v2.as_slice());

    // Verify deploy slot updated
    let pd_state = LoaderState::from_account_data(&pd_acc.data).unwrap();
    match pd_state {
        LoaderState::ProgramData { slot, .. } => assert_eq!(slot, 6),
        _ => panic!("expected ProgramData"),
    }
}

/// Deploying garbage bytes should fail WASM validation
#[test]
fn deploy_invalid_wasm_fails() {
    let (storage, _dir) = test_storage();
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();
    let cache = ProgramCache::new(16);

    let payer_kp = Keypair::generate();
    let payer = payer_kp.address();
    let buffer_kp = Keypair::generate();
    let buffer = buffer_kp.address();
    let program_kp = Keypair::generate();
    let program = program_kp.address();
    let program_data = hashv(&[b"program_data", program.as_bytes()]);
    let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];

    storage
        .put_account(
            &payer,
            0,
            &Account::new(100_000_000_000, *LOADER_PROGRAM_ID),
        )
        .unwrap();

    // Initialize buffer (buffer=signer, authority/payer=signer)
    let init_ix = nusantara_loader_program::initialize_buffer(&buffer, &payer);
    let msg = Message::new(&[init_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &buffer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 1, &result);

    // Write garbage (authority/payer=signer)
    let write_ix = nusantara_loader_program::write(&buffer, &payer, 0, garbage);
    let msg = Message::new(&[write_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 2, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, 2, &result);

    // Deploy should fail (payer=signer, program=signer, authority=signer)
    let deploy_ix =
        nusantara_loader_program::deploy(&payer, &program, &program_data, &buffer, &payer, 1024);
    let msg = Message::new(&[deploy_ix], &payer).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&payer_kp, &program_kp]);
    let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 3, &cache, None, false);
    assert!(result.status.is_err(), "deploy of invalid WASM should fail");
}
