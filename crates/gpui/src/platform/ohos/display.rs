use std::fmt::Debug;
use uuid::Uuid;

use openharmony_ability::OpenHarmonyApp;

use crate::{Bounds, DisplayId, Pixels, PlatformDisplay, Result, Size, point, px, size};

pub(crate) struct OhosDisplay {
    app: OpenHarmonyApp,
    id: DisplayId,
}

impl OhosDisplay {
    pub(crate) fn new(app: OpenHarmonyApp) -> Self {
        Self {
            app,
            id: DisplayId(0),
        }
    }
}

impl Debug for OhosDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OhosDisplay")
            .field("id", &self.id)
            .finish()
    }
}

impl PlatformDisplay for OhosDisplay {
    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> Result<Uuid> {
        // Generate a stable UUID for the display
        Ok(Uuid::from_bytes([
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ]))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        // Default bounds for OHOS display
        // In a real implementation, this would query the actual display size
        Bounds::new(point(px(0.0), px(0.0)), size(px(1080.0), px(1920.0)))
    }

    fn visible_bounds(&self) -> Bounds<Pixels> {
        // On OHOS, visible bounds are the same as full bounds
        self.bounds()
    }
}

