#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    pub name: &'static str,
    pub width: usize,
    pub height: usize,
}

impl DeviceProfile {
    pub const A5X_FAMILY: DeviceProfile = DeviceProfile {
        name: "A5X/A6X",
        width: 1404,
        height: 1872,
    };
    pub const A5X2: DeviceProfile = DeviceProfile {
        name: "A5X2 (Manta)",
        width: 1920,
        height: 2560,
    };
    pub const A6X2: DeviceProfile = DeviceProfile {
        name: "A6X2 (Nomad)",
        width: 1404,
        height: 1872,
    };
}

pub fn resolve_device_profile(apply_equipment: Option<&str>) -> DeviceProfile {
    match apply_equipment {
        Some("N5") => DeviceProfile::A5X2,
        Some("N6") => DeviceProfile::A6X2,
        // A5/A6X/A5X and unknown values currently default to the legacy dimensions.
        _ => DeviceProfile::A5X_FAMILY,
    }
}
