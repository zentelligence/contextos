use base64::Engine;

use super::*;

#[test]
fn decoded_size_is_exact_for_every_padding_length() {
    for length in [0, 1, 2, 3, 4, 5, 6, 1023, 1024, 1025] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![7_u8; length]);
        assert_eq!(
            decoded_size(&encoded),
            u64::try_from(length).unwrap_or(u64::MAX),
            "{length} bytes"
        );
    }
}

#[test]
fn decoded_size_never_exceeds_the_real_length_of_unpadded_or_truncated_input() {
    // `STANDARD` refuses both at decode; the size check must not refuse them
    // first for being larger than they are.
    assert_eq!(decoded_size("AAEC"), 3);
    assert_eq!(decoded_size("AAE"), 0);
    assert_eq!(decoded_size("AAECAw"), 3);
}
