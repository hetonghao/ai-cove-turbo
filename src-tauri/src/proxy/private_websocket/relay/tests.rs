use super::should_offload_private_encoding;
use crate::proxy::MIN_COMPRESSION_INPUT_BYTES;

#[test]
fn private_encoding_offload_decision_changes_at_threshold() {
    assert!(!should_offload_private_encoding(
        MIN_COMPRESSION_INPUT_BYTES - 1
    ));
    assert!(should_offload_private_encoding(MIN_COMPRESSION_INPUT_BYTES));
}
