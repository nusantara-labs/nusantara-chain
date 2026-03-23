// SHA3-512 (Keccak-f[1600]) PoH verification shader
// Each workgroup verifies one PoH entry by iterating hash = sha3_512(hash) for num_hashes times.
// WGSL has no u64, so we use pairs of u32 to represent the 25-word (u64) Keccak state.

// Entry layout in the buffer:
// [0..64]   = initial_hash (64 bytes)
// [64..72]  = num_hashes (u64 as 8 bytes LE)
// [72..136] = expected_hash (64 bytes)
// Total: 136 bytes per entry

struct PohEntry {
    initial_hash: array<u32, 16>,  // 64 bytes as 16 x u32
    num_hashes_lo: u32,
    num_hashes_hi: u32,
    expected_hash: array<u32, 16>, // 64 bytes as 16 x u32
}

@group(0) @binding(0)
var<storage, read> entries: array<PohEntry>;

@group(0) @binding(1)
var<storage, read_write> results: array<u32>;

// Keccak round constants (low and high halves of 24 u64 constants)
const RC_LO: array<u32, 24> = array<u32, 24>(
    0x00000001u, 0x00008082u, 0x0000808au, 0x80008000u,
    0x0000808bu, 0x80000001u, 0x80008081u, 0x00008009u,
    0x0000008au, 0x00000088u, 0x80008009u, 0x8000000au,
    0x8000808bu, 0x0000008bu, 0x00008089u, 0x00008003u,
    0x00008002u, 0x00000080u, 0x0000800au, 0x8000000au,
    0x80008081u, 0x00008080u, 0x80000001u, 0x80008008u,
);

const RC_HI: array<u32, 24> = array<u32, 24>(
    0x00000000u, 0x00000000u, 0x80000000u, 0x80000000u,
    0x00000000u, 0x00000000u, 0x80000000u, 0x80000000u,
    0x00000000u, 0x00000000u, 0x00000000u, 0x00000000u,
    0x00000000u, 0x80000000u, 0x80000000u, 0x80000000u,
    0x80000000u, 0x80000000u, 0x00000000u, 0x80000000u,
    0x80000000u, 0x80000000u, 0x00000000u, 0x80000000u,
);

// Rotation offsets for rho step
const RHO_OFFSETS: array<u32, 25> = array<u32, 25>(
    0u, 1u, 62u, 28u, 27u,
    36u, 44u, 6u, 55u, 20u,
    3u, 10u, 43u, 25u, 39u,
    41u, 45u, 15u, 21u, 8u,
    18u, 2u, 61u, 56u, 14u,
);

// Pi step permutation indices
const PI_INDICES: array<u32, 25> = array<u32, 25>(
    0u, 6u, 12u, 18u, 24u,
    3u, 9u, 10u, 16u, 22u,
    1u, 7u, 13u, 19u, 20u,
    4u, 5u, 11u, 17u, 23u,
    2u, 8u, 14u, 15u, 21u,
);

// Rotate a u64 represented as (lo, hi) pair by n bits
fn rot64(lo: u32, hi: u32, n: u32) -> vec2<u32> {
    let shift = n % 64u;
    if shift == 0u {
        return vec2<u32>(lo, hi);
    }
    if shift < 32u {
        let new_lo = (lo << shift) | (hi >> (32u - shift));
        let new_hi = (hi << shift) | (lo >> (32u - shift));
        return vec2<u32>(new_lo, new_hi);
    }
    let s = shift - 32u;
    if s == 0u {
        return vec2<u32>(hi, lo);
    }
    let new_lo = (hi << s) | (lo >> (32u - s));
    let new_hi = (lo << s) | (hi >> (32u - s));
    return vec2<u32>(new_lo, new_hi);
}

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let entry_idx = id.x;
    let entry = entries[entry_idx];

    // Initialize 1600-bit state (25 u64 = 50 u32)
    var state_lo: array<u32, 25>;
    var state_hi: array<u32, 25>;
    for (var i = 0u; i < 25u; i++) {
        state_lo[i] = 0u;
        state_hi[i] = 0u;
    }

    // Load initial hash into working buffer
    var current_hash: array<u32, 16>;
    for (var i = 0u; i < 16u; i++) {
        current_hash[i] = entry.initial_hash[i];
    }

    // WGSL has no u64, so we use num_hashes_lo directly.
    // For PoH verification, hash counts per entry fit in u32 (max ~12500 per tick).

    // Main PoH loop: hash = sha3_512(hash) repeated num_hashes times
    for (var iter = 0u; iter < entry.num_hashes_lo; iter++) {
        // Reset state
        for (var i = 0u; i < 25u; i++) {
            state_lo[i] = 0u;
            state_hi[i] = 0u;
        }

        // Absorb: XOR 64-byte input into first 8 u64 words of state
        // SHA3-512 rate = 576 bits = 72 bytes = 9 u64 words
        // But our input is exactly 64 bytes = 8 u64 words
        for (var i = 0u; i < 8u; i++) {
            state_lo[i] = state_lo[i] ^ current_hash[i * 2u];
            state_hi[i] = state_hi[i] ^ current_hash[i * 2u + 1u];
        }

        // Padding: SHA3 domain separation byte 0x06 and final bit 0x80
        // Byte 64 gets 0x06, byte 71 (last byte of rate) gets 0x80
        state_lo[8u] = state_lo[8u] ^ 0x00000006u;
        // Byte 71 is in word 8, high part byte 3 (71 = 8*8 + 7)
        state_hi[8u] = state_hi[8u] ^ 0x80000000u;

        // 24 rounds of Keccak-f[1600]
        for (var round = 0u; round < 24u; round++) {
            // Theta step
            var c_lo: array<u32, 5>;
            var c_hi: array<u32, 5>;
            for (var x = 0u; x < 5u; x++) {
                c_lo[x] = state_lo[x] ^ state_lo[x + 5u] ^ state_lo[x + 10u] ^ state_lo[x + 15u] ^ state_lo[x + 20u];
                c_hi[x] = state_hi[x] ^ state_hi[x + 5u] ^ state_hi[x + 10u] ^ state_hi[x + 15u] ^ state_hi[x + 20u];
            }
            for (var x = 0u; x < 5u; x++) {
                let r = rot64(c_lo[(x + 1u) % 5u], c_hi[(x + 1u) % 5u], 1u);
                let d_lo = c_lo[(x + 4u) % 5u] ^ r.x;
                let d_hi = c_hi[(x + 4u) % 5u] ^ r.y;
                for (var y = 0u; y < 5u; y++) {
                    state_lo[x + y * 5u] = state_lo[x + y * 5u] ^ d_lo;
                    state_hi[x + y * 5u] = state_hi[x + y * 5u] ^ d_hi;
                }
            }

            // Rho + Pi steps
            var temp_lo: array<u32, 25>;
            var temp_hi: array<u32, 25>;
            for (var i = 0u; i < 25u; i++) {
                let r = rot64(state_lo[i], state_hi[i], RHO_OFFSETS[i]);
                let pi = PI_INDICES[i];
                temp_lo[pi] = r.x;
                temp_hi[pi] = r.y;
            }

            // Chi step
            for (var y = 0u; y < 5u; y++) {
                for (var x = 0u; x < 5u; x++) {
                    let idx = x + y * 5u;
                    let idx1 = (x + 1u) % 5u + y * 5u;
                    let idx2 = (x + 2u) % 5u + y * 5u;
                    state_lo[idx] = temp_lo[idx] ^ ((~temp_lo[idx1]) & temp_lo[idx2]);
                    state_hi[idx] = temp_hi[idx] ^ ((~temp_hi[idx1]) & temp_hi[idx2]);
                }
            }

            // Iota step
            state_lo[0u] = state_lo[0u] ^ RC_LO[round];
            state_hi[0u] = state_hi[0u] ^ RC_HI[round];
        }

        // Squeeze: extract first 64 bytes (8 u64 words) as hash output
        for (var i = 0u; i < 8u; i++) {
            current_hash[i * 2u] = state_lo[i];
            current_hash[i * 2u + 1u] = state_hi[i];
        }
    }

    // Compare result with expected hash
    var match_result = 1u;
    for (var i = 0u; i < 16u; i++) {
        if current_hash[i] != entry.expected_hash[i] {
            match_result = 0u;
        }
    }

    results[entry_idx] = match_result;
}
