//! Layout of the Windows `USB_NODE_CONNECTION_INFORMATION_EX` reply
//! (usbioctl.h, 1-byte packed). Kept platform-free so the parsing is tested on
//! every host, not only on Windows.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

pub const CONNECTION_INFO_EX_HEADER_LEN: usize = 35;
const CONNECTION_STATUS_OFFSET: usize = 31;
pub const NO_DEVICE_CONNECTED: u32 = 0;

/// Request buffer for one hub port; the IOCTL reads `ConnectionIndex` from
/// the first four bytes and overwrites the rest.
pub fn connection_info_request(port: u32, buffer: &mut [u8]) {
    buffer.fill(0);
    buffer[..4].copy_from_slice(&port.to_le_bytes());
}

/// Raw `ConnectionStatus`; `None` for replies shorter than the header, so
/// callers keep the device. Only `NO_DEVICE_CONNECTED` means the port is empty.
pub fn connection_status(reply: &[u8]) -> Option<u32> {
    if reply.len() < CONNECTION_INFO_EX_HEADER_LEN {
        return None;
    }
    let status = reply.get(CONNECTION_STATUS_OFFSET..CONNECTION_STATUS_OFFSET + 4)?;
    Some(u32::from_le_bytes(status.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u32) -> Vec<u8> {
        let mut buffer = vec![0xAA; CONNECTION_INFO_EX_HEADER_LEN];
        buffer[CONNECTION_STATUS_OFFSET..CONNECTION_STATUS_OFFSET + 4]
            .copy_from_slice(&status.to_le_bytes());
        buffer
    }

    #[test]
    fn empty_port_is_reported_as_gone() {
        assert_eq!(connection_status(&reply(0)), Some(NO_DEVICE_CONNECTED));
    }

    #[test]
    fn connected_or_faulted_port_keeps_the_device() {
        // DeviceConnected = 1, DeviceFailedEnumeration = 2, overcurrent = 4.
        for status in [1, 2, 4] {
            assert_eq!(connection_status(&reply(status)), Some(status));
            assert_ne!(status, NO_DEVICE_CONNECTED);
        }
    }

    #[test]
    fn short_reply_is_inconclusive() {
        assert_eq!(connection_status(&[0; 20]), None);
    }

    #[test]
    fn request_carries_the_port_number() {
        let mut buffer = [0xFF; 64];
        connection_info_request(5, &mut buffer);
        assert_eq!(&buffer[..4], &[5, 0, 0, 0]);
        assert!(buffer[4..].iter().all(|byte| *byte == 0));
    }
}
