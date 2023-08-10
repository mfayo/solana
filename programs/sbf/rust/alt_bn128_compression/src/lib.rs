//! Alt_bn128 compression Syscalls tests

extern crate solana_program;
use solana_program::{
    alt_bn128_compression::prelude::{
        alt_bn128_compression_g1_compress, alt_bn128_compression_g1_decompress,
        alt_bn128_compression_g2_compress, alt_bn128_compression_g2_decompress,
    },
    custom_heap_default, custom_panic_default, msg,
};

fn alt_bn128_compression_g1() {
    let points_g1: [[u8; 64]; 3] = [
        [
            45, 206, 255, 166, 152, 55, 128, 138, 79, 217, 145, 164, 25, 74, 120, 234, 234, 217,
            68, 149, 162, 44, 133, 120, 184, 205, 12, 44, 175, 98, 168, 172, 20, 24, 216, 15, 209,
            175, 106, 75, 147, 236, 90, 101, 123, 219, 245, 151, 209, 202, 218, 104, 148, 8, 32,
            254, 243, 191, 218, 122, 42, 81, 193, 84,
        ],
        [
            45, 206, 255, 166, 152, 55, 128, 138, 79, 217, 145, 164, 25, 74, 120, 234, 234, 217,
            68, 149, 162, 44, 133, 120, 184, 205, 12, 44, 175, 98, 168, 172, 28, 75, 118, 99, 15,
            130, 53, 222, 36, 99, 235, 81, 5, 165, 98, 197, 197, 182, 144, 40, 212, 105, 169, 142,
            72, 96, 177, 156, 174, 43, 59, 243,
        ],
        [0u8; 64],
    ];
    points_g1.iter().for_each(|point| {
        let g1_compressed = alt_bn128_compression_g1_compress(point).unwrap();
        let g1_decompressed = alt_bn128_compression_g1_decompress(&g1_compressed).unwrap();
        assert_eq!(*point, g1_decompressed);
    });
}

fn alt_bn128_compression_g2() {
    let points_g2: [[u8; 128]; 3] = [
        [
            40, 57, 233, 205, 180, 46, 35, 111, 215, 5, 23, 93, 12, 71, 118, 225, 7, 46, 247, 147,
            47, 130, 106, 189, 184, 80, 146, 103, 141, 52, 242, 25, 0, 203, 124, 176, 110, 34, 151,
            212, 66, 180, 238, 151, 236, 189, 133, 209, 17, 137, 205, 183, 168, 196, 92, 159, 75,
            174, 81, 168, 18, 86, 176, 56, 16, 26, 210, 20, 18, 81, 122, 142, 104, 62, 251, 169,
            98, 141, 21, 253, 50, 130, 182, 15, 33, 109, 228, 31, 79, 183, 88, 147, 174, 108, 4,
            22, 14, 129, 168, 6, 80, 246, 254, 100, 218, 131, 94, 49, 247, 211, 3, 245, 22, 200,
            177, 91, 60, 144, 147, 174, 90, 17, 19, 189, 62, 147, 152, 18,
        ],
        [
            40, 57, 233, 205, 180, 46, 35, 111, 215, 5, 23, 93, 12, 71, 118, 225, 7, 46, 247, 147,
            47, 130, 106, 189, 184, 80, 146, 103, 141, 52, 242, 25, 0, 203, 124, 176, 110, 34, 151,
            212, 66, 180, 238, 151, 236, 189, 133, 209, 17, 137, 205, 183, 168, 196, 92, 159, 75,
            174, 81, 168, 18, 86, 176, 56, 32, 73, 124, 94, 206, 224, 37, 155, 80, 17, 74, 13, 30,
            244, 66, 96, 100, 254, 180, 130, 71, 3, 230, 109, 236, 105, 51, 131, 42, 16, 249, 49,
            33, 226, 166, 108, 144, 58, 161, 196, 221, 204, 231, 132, 137, 174, 84, 104, 128, 184,
            185, 54, 43, 225, 54, 222, 226, 15, 120, 89, 153, 233, 101, 53,
        ],
        [0u8; 128],
    ];
    points_g2.iter().for_each(|point| {
        let g2_compressed = alt_bn128_compression_g2_compress(point).unwrap();
        let g2_decompressed = alt_bn128_compression_g2_decompress(&g2_compressed).unwrap();
        assert_eq!(*point, g2_decompressed);
    });
}
#[no_mangle]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {
    msg!("alt_bn128_compression");

    alt_bn128_compression_g1();
    alt_bn128_compression_g2();
    0
}

custom_heap_default!();
custom_panic_default!();
