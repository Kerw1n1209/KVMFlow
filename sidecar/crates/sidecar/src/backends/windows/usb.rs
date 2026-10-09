//! Windows USB adapter - SetupAPI device-interface enumeration, polled into
//! snapshots by the shared poller. Instance IDs look like
//! `USB\VID_1A40&PID_0101\5&2B54EF7E&0&1`; the segment after the final `\`
//! is used as the serial field (may be an instance path on some devices,
//! which is why trigger matching never requires the serial).

use crate::backends::usb_port;
use crate::backends::UsbDevice;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, CM_Get_Device_IDW, CM_Get_Device_Interface_ListW, CM_Get_Parent,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDeviceRegistryPropertyW, CM_DEVNODE_STATUS_FLAGS,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_PROB, CR_SUCCESS, DIGCF_DEVICEINTERFACE,
    DIGCF_PRESENT, DN_DEVICE_DISCONNECTED, DN_STARTED, DN_WILL_BE_REMOVED, MAX_DEVICE_ID_LEN,
    SPDRP_ADDRESS, SPDRP_DEVICEDESC, SPDRP_FRIENDLYNAME, SPDRP_MFG, SPINT_ACTIVE, SPINT_REMOVED,
    SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Usb::IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX;
use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;

/// GUID_DEVINTERFACE_USB_DEVICE {A5DCBF10-6530-11D2-901F-00C04FB951ED}
const GUID_DEVINTERFACE_USB_DEVICE: windows::core::GUID =
    windows::core::GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);

/// GUID_DEVINTERFACE_USB_HUB {F18A0E88-C30C-11D0-8815-00A0C906BED8}
/// Some hubs register only this interface, so a USB Switch's hub can be
/// invisible to the snapshot unless it is enumerated too.
const GUID_DEVINTERFACE_USB_HUB: windows::core::GUID =
    windows::core::GUID::from_u128(0xF18A0E88_C30C_11D0_8815_00A0C906BED8);

/// Keys ever seen through the hub interface. A hub is usually listed under
/// both interfaces, so its plain USB-device entry must not keep it present
/// once the hub entry says it is gone.
static KNOWN_HUBS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// Last probe outcome per hub key, plus changes not yet reported.
static HUB_PROBES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
static HUB_PROBE_CHANGES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn usb_snapshot() -> Vec<UsbDevice> {
    let mut hub_outcomes = BTreeMap::new();
    let hubs = enumerate(&GUID_DEVINTERFACE_USB_HUB, Some(&mut hub_outcomes)).unwrap_or_default();
    let devices = enumerate(&GUID_DEVINTERFACE_USB_DEVICE, None).unwrap_or_default();

    let mut known = KNOWN_HUBS.lock().unwrap_or_else(|e| e.into_inner());
    known.extend(hub_outcomes.keys().cloned());
    for key in known.iter() {
        hub_outcomes
            .entry(key.clone())
            .or_insert_with(|| "hub_interface_missing".to_string());
    }
    let confirmed: BTreeSet<&String> = hubs.iter().map(|hub| &hub.key).collect();
    let mut all: Vec<UsbDevice> = hubs.to_vec();
    all.extend(
        devices
            .into_iter()
            .filter(|device| !known.contains(&device.key) || confirmed.contains(&device.key)),
    );
    drop(known);
    record_hub_probes(hub_outcomes);

    all.sort_by(|a, b| a.key.cmp(&b.key));
    all.dedup_by(|a, b| a.key == b.key);
    all
}

/// Probe outcomes that changed since the last call, for the session log.
pub fn take_hub_probe_changes() -> Vec<(String, String)> {
    std::mem::take(&mut *HUB_PROBE_CHANGES.lock().unwrap_or_else(|e| e.into_inner()))
}

fn record_hub_probes(outcomes: BTreeMap<String, String>) {
    let mut last = HUB_PROBES.lock().unwrap_or_else(|e| e.into_inner());
    let mut changes = HUB_PROBE_CHANGES.lock().unwrap_or_else(|e| e.into_inner());
    for (key, outcome) in outcomes {
        if last.get(&key) != Some(&outcome) {
            changes.push((key.clone(), outcome.clone()));
            last.insert(key, outcome);
        }
    }
}

fn enumerate(
    interface: &windows::core::GUID,
    mut hub_outcomes: Option<&mut BTreeMap<String, String>>,
) -> windows::core::Result<Vec<UsbDevice>> {
    let devinfo = unsafe {
        SetupDiGetClassDevsW(
            Some(interface),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )?
    };

    let mut devices = Vec::new();
    let mut index: u32 = 0;
    loop {
        let mut active_interface = SP_DEVICE_INTERFACE_DATA::default();
        active_interface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        if unsafe {
            SetupDiEnumDeviceInterfaces(devinfo, None, interface, index, &mut active_interface)
        }
        .is_err()
        {
            break;
        }
        index += 1;
        // Device-info entries can outlive a disconnected hub while Windows
        // tears it down. Only a still-active interface counts as presence.
        if active_interface.Flags & SPINT_ACTIVE == 0 || active_interface.Flags & SPINT_REMOVED != 0
        {
            continue;
        }
        let mut info = SP_DEVINFO_DATA::default();
        info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
        // Documented device-info-only query: ERROR_INSUFFICIENT_BUFFER is
        // expected with a null path buffer, but SP_DEVINFO_DATA is populated.
        let detail = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devinfo,
                &active_interface,
                None,
                0,
                None,
                Some(&mut info),
            )
        };
        if detail.is_err_and(|error| error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()) {
            continue;
        }
        let mut status = CM_DEVNODE_STATUS_FLAGS::default();
        let mut problem = CM_PROB::default();
        let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, info.DevInst, 0) };
        if result != CR_SUCCESS
            || status.0 & DN_STARTED.0 == 0
            || status.0 & (DN_DEVICE_DISCONNECTED.0 | DN_WILL_BE_REMOVED.0) != 0
        {
            continue;
        }

        // Instance id: "USB\VID_1A40&PID_0101\<serial-or-instance>"
        let mut buf = [0u16; 512];
        let ok = unsafe { SetupDiGetDeviceInstanceIdW(devinfo, &info, Some(&mut buf), None) };
        if ok.is_err() {
            continue;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let instance = String::from_utf16_lossy(&buf[..len]);
        let Some(parsed) = parse_instance(&instance) else {
            continue;
        };
        if let Some(outcomes) = hub_outcomes.as_deref_mut() {
            let probe = upstream_port_status(devinfo, &info);
            let empty = matches!(probe, Ok(status) if status == usb_port::NO_DEVICE_CONNECTED);
            outcomes.insert(
                parsed.key.clone(),
                match probe {
                    Ok(status) if empty => format!("port_empty({status})"),
                    Ok(status) => format!("port_connected({status})"),
                    Err(step) => format!("probe_failed({step})"),
                },
            );
            if empty {
                continue;
            }
        }
        devices.push(UsbDevice {
            product: registry_string(devinfo, &info, SPDRP_FRIENDLYNAME)
                .or_else(|| registry_string(devinfo, &info, SPDRP_DEVICEDESC))
                .unwrap_or_default(),
            vendor: registry_string(devinfo, &info, SPDRP_MFG).unwrap_or_default(),
            ..parsed
        });
    }

    let _ = unsafe { SetupDiDestroyDeviceInfoList(devinfo) };
    devices.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(devices)
}

/// After a USB Switch moves to another computer, Windows keeps the switch's
/// hub devnode started for ~2.5s while PnP tears the hub down. The parent
/// hub's port status follows the port-change event instead of that teardown,
/// so asking it directly avoids the delay. Returns the raw ConnectionStatus,
/// or the failed step so callers keep the hub and the log says why.
fn upstream_port_status(
    devinfo: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    info: &SP_DEVINFO_DATA,
) -> Result<u32, &'static str> {
    let port = registry_u32(devinfo, info, SPDRP_ADDRESS).ok_or("address")?;
    let mut parent = 0u32;
    if unsafe { CM_Get_Parent(&mut parent, info.DevInst, 0) } != CR_SUCCESS {
        return Err("parent");
    }
    let mut parent_id = [0u16; MAX_DEVICE_ID_LEN as usize + 1];
    if unsafe { CM_Get_Device_IDW(parent, &mut parent_id, 0) } != CR_SUCCESS {
        return Err("parent_id");
    }
    let mut interfaces = [0u16; 2048];
    let listed = unsafe {
        CM_Get_Device_Interface_ListW(
            &GUID_DEVINTERFACE_USB_HUB,
            PCWSTR(parent_id.as_ptr()),
            &mut interfaces,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if listed != CR_SUCCESS || interfaces[0] == 0 {
        return Err("parent_hub_interface");
    }
    let hub = unsafe {
        CreateFileW(
            PCWSTR(interfaces.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|_| "open_parent_hub")?;
    let mut buffer = [0u8; 2048];
    usb_port::connection_info_request(port, &mut buffer);
    let mut returned = 0u32;
    let queried = unsafe {
        DeviceIoControl(
            hub,
            IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
            Some(buffer.as_ptr().cast()),
            buffer.len() as u32,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    let _ = unsafe { CloseHandle(hub) };
    if queried.is_err() {
        return Err("ioctl");
    }
    usb_port::connection_status(&buffer[..returned as usize]).ok_or("short_reply")
}

fn registry_u32(
    devinfo: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: windows::Win32::Devices::DeviceAndDriverInstallation::SETUP_DI_REGISTRY_PROPERTY,
) -> Option<u32> {
    let mut bytes = [0u8; 4];
    let mut required_size = 0u32;
    unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            devinfo,
            info,
            property,
            None,
            Some(&mut bytes),
            Some(&mut required_size),
        )
        .ok()?;
    }
    (required_size == 4).then(|| u32::from_le_bytes(bytes))
}

fn registry_string(
    devinfo: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: windows::Win32::Devices::DeviceAndDriverInstallation::SETUP_DI_REGISTRY_PROPERTY,
) -> Option<String> {
    let mut bytes = [0u8; 2048];
    let mut required_size = 0u32;
    let mut registry_type = 0u32;
    unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            devinfo,
            info,
            property,
            Some(&mut registry_type),
            Some(&mut bytes),
            Some(&mut required_size),
        )
        .ok()?;
    }
    let length = (required_size as usize).min(bytes.len());
    let value = String::from_utf16_lossy(
        &bytes[..length]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect::<Vec<_>>(),
    );
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn parse_instance(instance: &str) -> Option<UsbDevice> {
    let rest = instance.strip_prefix("USB\\")?;
    let (ids, serial) = match rest.split_once('\\') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let mut vid = String::new();
    let mut pid = String::new();
    for part in ids.split('&') {
        let upper = part.to_ascii_uppercase();
        if let Some(v) = upper.strip_prefix("VID_") {
            vid = v.to_ascii_lowercase();
        } else if let Some(p) = upper.strip_prefix("PID_") {
            pid = p.to_ascii_lowercase();
        }
    }
    if vid.len() != 4 || pid.len() != 4 {
        return None;
    }
    let vid_pid = format!("{vid}:{pid}");
    Some(UsbDevice {
        key: format!("{vid_pid}:{serial}"),
        vid_pid,
        serial,
        product: String::new(),
        vendor: String::new(),
    })
}
