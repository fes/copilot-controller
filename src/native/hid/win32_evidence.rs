//! Pure validation of exact SetupAPI evidence; no native calls.

use super::{HidBus, HidDescriptor, HidError};

const GUID_LEN: usize = 16;
const DEVPROP_TYPE_GUID: u32 = 13;
// GUID_BUS_TYPE_USB from Wdmguid.h, in the native GUID memory layout.
const USB_GUID: [u8; GUID_LEN] = [
    0xbc, 0xeb, 0x7d, 0x9d, 0x5d, 0xc8, 0xd1, 0x11, 0x9e, 0xb4, 0x00, 0x60, 0x08, 0xc3, 0xa1, 0x9a,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BusEvidence(Option<[u8; GUID_LEN]>);

impl BusEvidence {
    pub(super) fn read(
        get_property: impl FnOnce(&mut [u8; GUID_LEN]) -> Option<(u32, usize)>,
    ) -> Self {
        let mut bytes = [0; GUID_LEN];
        let guid = get_property(&mut bytes)
            .and_then(|(ty, len)| (ty == DEVPROP_TYPE_GUID && len == bytes.len()).then_some(bytes));
        Self(guid)
    }

    pub(super) fn classification(self) -> Option<HidBus> {
        self.0.map(|guid| {
            if guid == USB_GUID {
                HidBus::Usb
            } else {
                HidBus::Unknown
            }
        })
    }
}

#[derive(Clone)]
pub(super) struct WindowsEvidence {
    pub(super) descriptor: HidDescriptor,
    pub(super) bus: BusEvidence,
}

/// The same property/handle readers run again after CreateFileW. Preserve the
/// raw GUID too: two different unsupported buses must not compare as Unknown.
pub(super) fn revalidate_open(
    expected: &WindowsEvidence,
    allow_usage_change: bool,
    read: impl FnOnce() -> Result<WindowsEvidence, HidError>,
) -> Result<(), HidError> {
    let mut actual = read()?;
    if allow_usage_change {
        actual.descriptor.usage_page = expected.descriptor.usage_page;
        actual.descriptor.usage = expected.descriptor.usage;
    }
    if actual.descriptor != expected.descriptor || actual.bus != expected.bus {
        return Err(HidError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::hid::{HidIdentity, HidPath};

    fn property(guid: [u8; 16], ty: u32, len: usize, success: bool) -> BusEvidence {
        BusEvidence::read(|buffer| {
            buffer.copy_from_slice(&guid);
            success.then_some((ty, len))
        })
    }

    fn evidence(guid: [u8; 16]) -> WindowsEvidence {
        let bus = property(guid, DEVPROP_TYPE_GUID, 16, true);
        WindowsEvidence {
            descriptor: HidDescriptor {
                identity: HidIdentity {
                    vendor_id: 0x0300,
                    product_id: 0x3010,
                },
                usage_page: 0xff00,
                usage: 1,
                product: Some("USB Bluetooth mi_00".into()),
                serial: None,
                max_input_report_len: 64,
                max_output_report_len: 64,
                interface_number: None,
                bus: bus.classification(),
                path: HidPath::new(r"\\?\hid#vid_0300&pid_3010&mi_00").unwrap(),
            },
            bus,
        }
    }

    #[test]
    fn bus_requires_success_exact_guid_type_and_size() {
        assert_eq!(
            property(USB_GUID, DEVPROP_TYPE_GUID, 16, true).classification(),
            Some(HidBus::Usb)
        );
        for (ty, len, success) in [
            (0, 16, true),
            (18, 16, true),                         // DEVPROP_TYPE_STRING
            (0x1000 | DEVPROP_TYPE_GUID, 16, true), // An array is not a GUID.
            (DEVPROP_TYPE_GUID, 0, true),
            (DEVPROP_TYPE_GUID, 15, true),
            (DEVPROP_TYPE_GUID, 17, true),
            (DEVPROP_TYPE_GUID, usize::MAX, true),
            (DEVPROP_TYPE_GUID, 16, false),
        ] {
            assert_eq!(property(USB_GUID, ty, len, success).classification(), None);
        }
        assert_eq!(BusEvidence::read(|_| None).classification(), None);
    }

    #[test]
    fn unknown_buses_and_bluetooth_class_guid_are_not_transport_evidence() {
        // GUID_DEVCLASS_BLUETOOTH is a setup class, not a documented bus GUID.
        let bluetooth_class = [
            0x6c, 0xf0, 0xcb, 0xe0, 0x8b, 0xcd, 0x47, 0x46, 0xbb, 0x8a, 0x26, 0x3b, 0x43, 0xf0,
            0xf9, 0x74,
        ];
        for guid in [[0; 16], [0xff; 16], bluetooth_class] {
            assert_eq!(evidence(guid).descriptor.bus, Some(HidBus::Unknown));
        }
    }

    #[test]
    fn revalidation_rejects_identity_metadata_and_raw_bus_changes() {
        let expected = evidence(USB_GUID);
        revalidate_open(&expected, false, || Ok(expected.clone())).unwrap();
        let mut changes = Vec::new();
        let mut changed = expected.clone();
        changed.descriptor.identity.product_id += 1;
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.serial = Some("replacement".into());
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.max_output_report_len += 1;
        changes.push(changed);
        let mut changed = expected.clone();
        changed.descriptor.interface_number = Some(0);
        changes.push(changed);
        let mut changed = expected.clone();
        changed.bus = BusEvidence::read(|_| None);
        changed.descriptor.bus = None;
        changes.push(changed);
        changes.push(evidence([0xff; 16]));
        for changed in changes {
            assert_eq!(
                revalidate_open(&expected, true, || Ok(changed)),
                Err(HidError::NotFound)
            );
        }
        assert_eq!(
            revalidate_open(&evidence([0; 16]), false, || Ok(evidence([0xff; 16]))),
            Err(HidError::NotFound)
        );
        assert_eq!(
            revalidate_open(&expected, false, || Err(HidError::NotFound)),
            Err(HidError::NotFound)
        );
    }

    #[test]
    fn composite_reopen_may_change_only_usage() {
        let expected = evidence(USB_GUID);
        let mut actual = expected.clone();
        actual.descriptor.usage_page = 1;
        actual.descriptor.usage = 2;
        assert_eq!(
            revalidate_open(&expected, false, || Ok(actual.clone())),
            Err(HidError::NotFound)
        );
        revalidate_open(&expected, true, || Ok(actual)).unwrap();
    }
}
