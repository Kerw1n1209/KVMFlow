//! DDC/CI VCP code table - single copy for all platform adapters.
//!
//! Codes are written as hex literals because the DDC/CI specs express them
//! in hex ("Input Select 60h"). KVM-8: the Windows adapter once carried
//! `VCP_INPUT = 60` decimal (= 0x3C), silently probing/switching the wrong
//! feature on every DDC transaction; the table + pinned test below exist
//! so that class of mistake cannot compile its way past a test run again.
//! (The macOS adapter needs no numeric code - m1ddc's `input` argument is
//! named - but any future numeric use must come from here.)

/// VCP 0x60 (96): Input Select. The one feature KVMFlow reads/writes.
pub const INPUT_SOURCE: u8 = 0x60;

#[cfg(test)]
mod tests {
    use super::*;

    /// KVM-8 regression: the code must be 0x60, i.e. decimal 96 - never the
    /// decimal misreading 60 (0x3C).
    #[test]
    fn input_source_code_is_0x60() {
        assert_eq!(
            INPUT_SOURCE, 0x60,
            "VCP code must be the hex 0x60 from the DDC/CI spec"
        );
        assert_eq!(
            INPUT_SOURCE, 96,
            "0x60 is decimal 96 - guards against the 60-decimal misreading"
        );
        assert_ne!(
            INPUT_SOURCE, 60,
            "decimal 60 is 0x3C, the KVM-8 defect value"
        );
    }

    /// The whole table must stay hex-canonical: every published code is
    /// pinned to its spec value in both notations, so adding a code without
    /// a pin (or with a decimal transcription) fails here.
    #[test]
    fn vcp_table_entries_are_pinned_to_spec_values() {
        let table: &[(&str, u8, u8)] = &[
            // (name, spec hex, spec decimal)
            ("INPUT_SOURCE", INPUT_SOURCE, 0x60),
        ];
        for (name, code, decimal) in table {
            assert_eq!(*code, *decimal, "{name}: hex and decimal pins must agree");
            assert_eq!(*code, 0x60, "{name}: spec value");
        }
    }
}
