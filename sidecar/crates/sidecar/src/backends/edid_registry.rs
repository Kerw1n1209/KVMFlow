//! Windows registry EDID resolution model.
//!
//! Pure string/byte logic with no Win32 dependency, so the Windows
//! enumeration path is unit-testable on any host (the KVM-7 defect - zero
//! displays on real Windows - shipped precisely because this logic had no
//! test seam). The FFI half lives in `windows/ddc.rs`.
//!
//! Key fact from the KVM-1 hardware probe (probes/windows/DisplayProbe.ps1,
//! validated on real machines): `EnumDisplayDevices` level-2 reports the
//! monitor as `MONITOR\<hwid>\<instance>`, but the EDID registry key lives
//! under `Enum\DISPLAY\<hwid>\<instance>` - a different enumerator prefix.
//! WmiMonitorID instance names append a `_<n>` suffix on top. Any single
//! hardcoded path shape silently drops monitors; we walk candidates instead.

/// Identity fields extracted from the first 128-byte EDID block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdidIdentity {
    pub manufacturer: String,
    pub product_code: u16,
    pub serial_number: u32,
    pub serial_string: String,
    pub model_name: String,
}

/// Strip the WMI-style `_<digits>` suffix WmiMonitorID appends to instance
/// names (`...UID264_0` -> `...UID264`). Instance IDs contain no other
/// trailing-digit-after-underscore pattern, so this is unambiguous.
fn strip_wmi_suffix(instance: &str) -> &str {
    if let Some(pos) = instance.rfind('_') {
        let tail = &instance[pos + 1..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return &instance[..pos];
        }
    }
    instance
}

/// Ordered registry subkey candidates (relative to HKLM) that may hold
/// `Device Parameters\EDID` for the monitor behind an EnumDisplayDevices
/// level-2 `DeviceID`.
///
/// Order = likelihood on real hardware (KVM-1 probe + KVM-6 forensic B4):
/// 1. `DISPLAY\<hwid>\<instance>` - the confirmed layout;
/// 2. the DeviceID verbatim - machines that genuinely key under `MONITOR\`;
/// 3. `DISPLAY\<hwid>\<instance>` with the WMI `_<n>` suffix stripped.
pub fn edid_key_candidates(device_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = device_id.splitn(3, '\\').collect();
    if parts.len() == 3 && !parts[1].is_empty() && !parts[2].is_empty() {
        let (prefix, hwid, instance) = (parts[0], parts[1], parts[2]);
        let base = "SYSTEM\\CurrentControlSet\\Enum";
        out.push(format!(r"{base}\DISPLAY\{hwid}\{instance}"));
        if prefix != "DISPLAY" {
            out.push(format!(r"{base}\{device_id}"));
        }
        let trimmed = strip_wmi_suffix(instance);
        if trimmed != instance {
            out.push(format!(r"{base}\DISPLAY\{hwid}\{trimmed}"));
        }
    } else {
        out.push(format!(r"SYSTEM\CurrentControlSet\Enum\{device_id}"));
    }
    out
}

/// Hardware-id segment of a DeviceID (`MONITOR\SAC2763\...` -> `SAC2763`),
/// used for the sibling-instance scan when no exact candidate resolves.
pub fn hardware_id_of(device_id: &str) -> Option<&str> {
    let parts: Vec<&str> = device_id.splitn(3, '\\').collect();
    match parts.as_slice() {
        [_, hwid, _] if !hwid.is_empty() => Some(hwid),
        _ => None,
    }
}

/// Parse the first 128-byte EDID block (moved verbatim from the Windows
/// adapter when this module was extracted; semantics unchanged).
pub fn parse_edid_128(e: &[u8]) -> Option<EdidIdentity> {
    if e.len() < 128 {
        return None;
    }
    if e[0] != 0x00 || e[1] != 0xFF || e[2] != 0xFF || e[3] != 0xFF {
        return None;
    }
    let manufacturer = {
        let b0 = e[8] as u32;
        let b1 = e[9] as u32;
        let l1 = ((b0 >> 2) & 0x1F) as u8;
        let l2 = (((b0 & 0x03) << 3) | ((b1 >> 5) & 0x07)) as u8;
        let l3 = (b1 & 0x1F) as u8;
        [l1, l2, l3]
            .iter()
            .map(|&v| (64 + v) as char)
            .collect::<String>()
    };
    let product_code = (e[10] as u16) | ((e[11] as u16) << 8);
    let serial_number =
        (e[12] as u32) | ((e[13] as u32) << 8) | ((e[14] as u32) << 16) | ((e[15] as u32) << 24);
    let mut model_name = String::new();
    let mut serial_string = String::new();
    for base in [54usize, 72, 90, 108] {
        if e[base] != 0 || e[base + 1] != 0 {
            continue;
        }
        let flag = e[base + 3];
        let text: String = e[base + 5..base + 18]
            .iter()
            .take_while(|&&c| c != 0x0A && c != 0)
            .map(|&c| c as char)
            .collect();
        let text = text.trim().to_string();
        if flag == 0xFC && model_name.is_empty() {
            model_name = text.clone();
        }
        if flag == 0xFF && serial_string.is_empty() {
            serial_string = text;
        }
    }
    Some(EdidIdentity {
        manufacturer,
        product_code,
        serial_number,
        serial_string,
        model_name,
    })
}

/// Resolve one monitor's EDID identity.
///
/// `read_edid(subkey)` returns the raw `Device Parameters\EDID` bytes for a
/// device-instance subkey (None if absent/unreadable); `list_keys(key)`
/// lists child subkey names. `used` carries the 128-byte blocks already
/// handed out this enumeration so the sibling scan cannot match two GDI
/// devices to the same registry instance (two same-model twins must stay
/// two distinct displays when their EDIDs differ).
pub fn resolve_edid(
    device_id: &str,
    read_edid: &dyn Fn(&str) -> Option<Vec<u8>>,
    list_keys: &dyn Fn(&str) -> Vec<String>,
    used: &mut Vec<[u8; 128]>,
) -> Result<EdidIdentity, String> {
    let mut trace: Vec<String> = Vec::new();
    let mut subkeys: Vec<String> = edid_key_candidates(device_id);
    // Sibling scan (KVM-1 probe fallback): exact instance keys missing but
    // other instances of the same hardware id carry EDID.
    if let Some(hwid) = hardware_id_of(device_id) {
        let base = format!(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{hwid}");
        let mut siblings: Vec<String> = list_keys(&base)
            .into_iter()
            .map(|inst| format!(r"{base}\{inst}"))
            .collect();
        siblings.retain(|s| !subkeys.iter().any(|c| c.eq_ignore_ascii_case(s)));
        subkeys.extend(siblings);
    }
    for subkey in &subkeys {
        let Some(raw) = read_edid(subkey) else {
            trace.push(format!("{subkey}: no EDID"));
            continue;
        };
        let Some(block) = raw_first_block(&raw) else {
            trace.push(format!("{subkey}: EDID too short ({})", raw.len()));
            continue;
        };
        let Some(identity) = parse_edid_128(&block) else {
            trace.push(format!("{subkey}: EDID header invalid"));
            continue;
        };
        if used.iter().any(|u| *u == block) {
            trace.push(format!("{subkey}: EDID already matched to another display"));
            continue;
        }
        used.push(block);
        return Ok(identity);
    }
    Err(format!(
        "no readable EDID for {device_id}; tried: {}",
        trace.join("; ")
    ))
}

fn raw_first_block(raw: &[u8]) -> Option<[u8; 128]> {
    if raw.len() < 128 {
        return None;
    }
    let mut block = [0u8; 128];
    block.copy_from_slice(&raw[..128]);
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Synthetic 128-byte EDID: manufacturer/product/serial + one 0xFC model
    /// descriptor and one 0xFF serial-string descriptor.
    fn edid(mfr: [u8; 2], product: u16, serial: u32, model: &str, serial_str: &str) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[0] = 0x00;
        e[1] = 0xFF;
        e[2] = 0xFF;
        e[3] = 0xFF;
        e[8] = mfr[0];
        e[9] = mfr[1];
        e[10] = (product & 0xFF) as u8;
        e[11] = (product >> 8) as u8;
        e[12] = (serial & 0xFF) as u8;
        e[13] = ((serial >> 8) & 0xFF) as u8;
        e[14] = ((serial >> 16) & 0xFF) as u8;
        e[15] = ((serial >> 24) & 0xFF) as u8;
        // descriptor 1 (base 54): monitor name - tag at base+3, text at base+5
        let name: Vec<u8> = model.bytes().take(13).collect();
        e[59..59 + name.len()].copy_from_slice(&name);
        e[57] = 0xFC;
        // descriptor 2 (base 72): serial string
        let s: Vec<u8> = serial_str.bytes().take(13).collect();
        e[77..77 + s.len()].copy_from_slice(&s);
        e[75] = 0xFF;
        e
    }

    /// "SAC" packed into the 2-byte EDID manufacturer field
    /// (5 bits per letter, A=1: S=19, A=1, C=3).
    fn sac() -> [u8; 2] {
        [0x4C, 0x23]
    }

    // ---- KVM-7 regression: the MONITOR\ -> DISPLAY\ registry prefix ----

    #[test]
    fn candidates_swap_monitor_prefix_to_display_first() {
        // Exact strings from the KVM-6 forensic session (Runner B4 + KVM-1
        // probe): EnumDisplayDevices says MONITOR\..., registry keys are
        // under Enum\DISPLAY\... The old code built only the MONITOR\ path
        // and dropped every monitor.
        let c = edid_key_candidates(r"MONITOR\SAC2763\7&1242ef3c&0&UID264");
        assert_eq!(
            c[0],
            r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763\7&1242ef3c&0&UID264"
        );
        assert!(c.contains(&format!(
            r"SYSTEM\CurrentControlSet\Enum\MONITOR\SAC2763\7&1242ef3c&0&UID264"
        )));
    }

    #[test]
    fn candidates_pass_display_prefixed_ids_through() {
        let c = edid_key_candidates(r"DISPLAY\SAC2466\7&1242ef3c&0&UID256");
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0],
            r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2466\7&1242ef3c&0&UID256"
        );
    }

    #[test]
    fn candidates_strip_wmi_suffix_variant() {
        let c = edid_key_candidates(r"MONITOR\SAC2763\7&1242ef3c&0&UID264_0");
        assert_eq!(
            c[0],
            r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763\7&1242ef3c&0&UID264_0"
        );
        assert!(c.contains(&format!(
            r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763\7&1242ef3c&0&UID264"
        )));
    }

    // ---- resolver against a mocked registry ----

    fn mock_registry(
        edids: &[(&str, Vec<u8>)],
        subkeys: &[(&str, Vec<&str>)],
    ) -> (HashMap<String, Vec<u8>>, HashMap<String, Vec<String>>) {
        let mut values = HashMap::new();
        for (k, v) in edids {
            values.insert(k.to_string(), v.clone());
        }
        let mut children = HashMap::new();
        for (k, v) in subkeys {
            children.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
        }
        (values, children)
    }

    /// KVM-6 home-lab replay: two different models, DeviceIDs MONITOR\...,
    /// registry EDID only under DISPLAY\... (exact Runner B4 layout).
    #[test]
    fn resolves_both_home_lab_monitors_from_display_keys() {
        let g73 = edid(sac(), 0x2763, 0x0000AB01, "G73", "0000000000001");
        let g52 = edid(sac(), 0x2466, 0x0000AB02, "G52plus", "0000000000000");
        let (values, children) = mock_registry(
            &[
                (
                    r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763\7&1242ef3c&0&UID264",
                    g73,
                ),
                (
                    r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2466\7&1242ef3c&0&UID256",
                    g52,
                ),
            ],
            &[],
        );
        let read = |k: &str| values.get(k).cloned();
        let list = |k: &str| children.get(k).cloned().unwrap_or_default();
        let mut used = Vec::new();

        let a = resolve_edid(
            r"MONITOR\SAC2763\7&1242ef3c&0&UID264",
            &read,
            &list,
            &mut used,
        );
        let b = resolve_edid(
            r"MONITOR\SAC2466\7&1242ef3c&0&UID256",
            &read,
            &list,
            &mut used,
        );
        let a = a.expect("G73 resolves");
        let b = b.expect("G52plus resolves");
        assert_eq!(a.model_name, "G73");
        assert_eq!(b.model_name, "G52plus");
        assert_ne!(a.serial_number, b.serial_number);
    }

    /// Two same-model monitors whose exact instance keys are absent from the
    /// registry: the sibling scan must still yield two distinct identities
    /// (different EDID serials), never one display eating both instances.
    #[test]
    fn same_model_twins_resolve_via_sibling_scan_distinctly() {
        let hw = r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763";
        let twin_a = edid(sac(), 0x2763, 0x11111111, "G73", "AAAAAAAAAAAA1");
        let twin_b = edid(sac(), 0x2763, 0x22222222, "G73", "AAAAAAAAAAAA2");
        let (values, children) = mock_registry(
            &[
                (&format!(r"{hw}\6&2ca91f6&0&UID7680"), twin_a),
                (&format!(r"{hw}\6&2ca91f6&0&UID8960"), twin_b),
            ],
            &[(hw, vec!["6&2ca91f6&0&UID7680", "6&2ca91f6&0&UID8960"])],
        );
        let read = |k: &str| values.get(k).cloned();
        let list = |k: &str| children.get(k).cloned().unwrap_or_default();
        let mut used = Vec::new();

        let a = resolve_edid(
            r"MONITOR\SAC2763\5&1000f6&0&UID1000",
            &read,
            &list,
            &mut used,
        )
        .expect("twin A via sibling scan");
        let b = resolve_edid(
            r"MONITOR\SAC2763\5&1000f6&0&UID2000",
            &read,
            &list,
            &mut used,
        )
        .expect("twin B via sibling scan");
        assert_ne!(
            a.serial_number, b.serial_number,
            "twins must keep distinct identities"
        );
        assert_ne!(a.serial_string, b.serial_string);
    }

    /// Byte-identical EDIDs (same model, no distinct serial): only one can be
    /// claimed; the second must fail loudly, not alias the first.
    #[test]
    fn identical_edid_blocks_are_not_handed_out_twice() {
        let block = edid(sac(), 0x2763, 0x33333333, "G73", "BBBBBBBBBBBB1");
        let hw = r"SYSTEM\CurrentControlSet\Enum\DISPLAY\SAC2763";
        let (values, children) = mock_registry(
            &[
                (&format!(r"{hw}\i1"), block.clone()),
                (&format!(r"{hw}\i2"), block.clone()),
            ],
            &[(hw, vec!["i1", "i2"])],
        );
        let read = |k: &str| values.get(k).cloned();
        let list = |k: &str| children.get(k).cloned().unwrap_or_default();
        let mut used = Vec::new();
        assert!(resolve_edid(r"MONITOR\SAC2763\x1", &read, &list, &mut used).is_ok());
        let second = resolve_edid(r"MONITOR\SAC2763\x2", &read, &list, &mut used);
        assert!(second.is_err());
        assert!(second.unwrap_err().contains("already matched"));
    }

    #[test]
    fn as_is_monitor_key_is_used_when_it_exists() {
        let block = edid(sac(), 0x2763, 7, "G73", "S1");
        let (values, children) = mock_registry(
            &[(
                r"SYSTEM\CurrentControlSet\Enum\MONITOR\OLD0001\inst1",
                block,
            )],
            &[],
        );
        let read = |k: &str| values.get(k).cloned();
        let list = |k: &str| children.get(k).cloned().unwrap_or_default();
        let mut used = Vec::new();
        assert!(resolve_edid(r"MONITOR\OLD0001\inst1", &read, &list, &mut used).is_ok());
    }

    #[test]
    fn failure_names_every_candidate_tried() {
        let (values, children) = mock_registry(&[], &[]);
        let read = |k: &str| values.get(k).cloned();
        let list = |k: &str| children.get(k).cloned().unwrap_or_default();
        let mut used = Vec::new();
        let err = resolve_edid(
            r"MONITOR\SAC2763\7&1242ef3c&0&UID264",
            &read,
            &list,
            &mut used,
        )
        .unwrap_err();
        assert!(
            err.contains("MONITOR\\SAC2763"),
            "diag echoes the device id: {err}"
        );
        assert!(
            err.contains("DISPLAY\\SAC2763"),
            "diag names the DISPLAY candidate: {err}"
        );
    }

    // ---- EDID parsing ----

    #[test]
    fn parses_manufacturer_product_serial_and_descriptors() {
        let e = edid(sac(), 0x2763, 0xDEADBEEF, "G73", "0000000000001");
        let id = parse_edid_128(&e).expect("parses");
        assert_eq!(id.manufacturer, "SAC");
        assert_eq!(id.product_code, 0x2763);
        assert_eq!(id.serial_number, 0xDEADBEEF);
        assert_eq!(id.model_name, "G73");
        assert_eq!(id.serial_string, "0000000000001");
    }

    #[test]
    fn rejects_short_or_headerless_blocks() {
        assert!(parse_edid_128(&[0u8; 64]).is_none());
        let mut e = edid(sac(), 1, 1, "X", "Y");
        e[1] = 0x00; // break the FF FF FF header
        assert!(parse_edid_128(&e).is_none());
    }

    #[test]
    fn same_model_different_serial_yields_distinct_stable_ids() {
        let a = parse_edid_128(&edid(sac(), 0x2763, 1, "G73", "A")).unwrap();
        let b = parse_edid_128(&edid(sac(), 0x2763, 2, "G73", "B")).unwrap();
        let ida = kvmflow_core::edid_identity(
            &a.manufacturer,
            a.product_code,
            a.serial_number,
            &a.serial_string,
        );
        let idb = kvmflow_core::edid_identity(
            &b.manufacturer,
            b.product_code,
            b.serial_number,
            &b.serial_string,
        );
        assert_ne!(ida, idb);
        assert_eq!(ida, "SAC-2763-00000001");
    }
}
