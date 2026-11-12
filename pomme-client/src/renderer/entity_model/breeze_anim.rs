//! `BreezeAnimation` keyframe tables, converted mechanically from the
//! game's own definitions (`scaleVec` values stored as `v - 1`).

use super::{KfAnim, KfChannel, KfTarget, Kfi, kf};

pub(super) static BREEZE_IDLE: KfAnim = KfAnim {
    length: 2.0,
    looping: true,
    channels: &[
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.5, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 0.5, 0.0, -0.5, Kfi::Lin),
                kf(0.75, -0.5, 0.0, -0.5, Kfi::Lin),
                kf(1.25, -0.5, 0.0, 0.5, Kfi::Lin),
                kf(1.75, 0.5, 0.0, 0.5, Kfi::Lin),
                kf(2.0, 0.5, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.5, 0.0, -0.5, Kfi::Lin),
                kf(0.5, -0.5, 0.0, -0.5, Kfi::Lin),
                kf(1.0, -0.5, 0.0, 0.5, Kfi::Lin),
                kf(1.5, 0.5, 0.0, 0.5, Kfi::Lin),
                kf(2.0, 0.5, 0.0, -0.5, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "head",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Cat),
                kf(1.0, 0.0, 1.0, 0.0, Kfi::Cat),
                kf(2.0, 0.0, 0.0, 0.0, Kfi::Cat),
            ],
        },
        KfChannel {
            part: "rods",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(2.0, 0.0, 1080.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "rods",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(1.0, 0.0, -1.0, 0.0, Kfi::Lin),
                kf(2.0, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
    ],
};

pub(super) static BREEZE_SHOOT: KfAnim = KfAnim {
    length: 1.125,
    looping: false,
    channels: &[
        KfChannel {
            part: "head",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, -12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.75, -12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.9167, 5.0, 0.0, 0.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "head",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 0.0, -2.0, 0.0, Kfi::Lin),
                kf(0.7917, 0.0, -1.0, 2.0, Kfi::Lin),
                kf(0.9583, 0.0, -1.0, 0.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_bottom",
            target: KfTarget::Rot,
            frames: &[kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin)],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.75, 12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.9167, -10.0, 0.0, 0.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 0.0, 0.0, 5.0, Kfi::Lin),
                kf(0.75, 0.0, 0.0, 6.0, Kfi::Lin),
                kf(0.9167, 0.0, 0.0, -2.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 15.0, 0.0, 0.0, Kfi::Lin),
                kf(0.75, 15.0, 0.0, 0.0, Kfi::Lin),
                kf(0.9167, -10.0, 0.0, 0.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 0.0, 0.0, 3.0, Kfi::Lin),
                kf(0.8333, 0.0, 0.0, 4.0, Kfi::Lin),
                kf(0.9583, 0.0, 0.0, -2.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "body",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.75, 12.5, 0.0, 0.0, Kfi::Lin),
                kf(0.9167, -2.5, 0.0, 0.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "body",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.25, 0.0, 3.0, 5.0, Kfi::Lin),
                kf(0.8333, 0.0, 3.0, 6.0, Kfi::Lin),
                kf(0.9583, 0.0, 3.0, -1.0, Kfi::Lin),
                kf(1.125, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "rods",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(1.0, 0.0, 360.0, 0.0, Kfi::Lin),
            ],
        },
    ],
};

pub(super) static BREEZE_JUMP: KfAnim = KfAnim {
    length: 0.5,
    looping: false,
    channels: &[
        KfChannel {
            part: "body",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, -10.0, 0.0, Kfi::Lin),
                kf(0.125, 0.0, 11.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "head",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 22.5, 0.0, 0.0, Kfi::Lin),
                kf(0.2083, -19.25, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_body",
            target: KfTarget::Scale,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.125, 0.0, 0.3, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_bottom",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 90.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 360.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_bottom",
            target: KfTarget::Scale,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.125, 0.0, 0.1, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 180.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, -6.0, 0.0, Kfi::Lin),
                kf(0.125, 0.0, 2.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 90.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, -5.0, 0.0, Kfi::Lin),
                kf(0.125, 0.0, 2.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "rods",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 360.0, 0.0, Kfi::Lin),
            ],
        },
    ],
};

pub(super) static BREEZE_INHALE: KfAnim = KfAnim {
    length: 2.0,
    looping: false,
    channels: &[
        KfChannel {
            part: "body",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, -10.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, -10.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "head",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 22.5, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 22.5, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_body",
            target: KfTarget::Scale,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_bottom",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 90.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_bottom",
            target: KfTarget::Scale,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, -6.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, -6.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.5, 0.0, -5.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, -5.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "rods",
            target: KfTarget::Rot,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.625, 0.0, 360.0, 0.0, Kfi::Lin),
            ],
        },
    ],
};

pub(super) static BREEZE_SLIDE: KfAnim = KfAnim {
    length: 0.2,
    looping: false,
    channels: &[
        KfChannel {
            part: "body",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.2, 0.0, 0.0, -6.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.2, 0.0, 0.0, -3.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, 0.0, Kfi::Lin),
                kf(0.2, 0.0, 0.0, -2.0, Kfi::Lin),
            ],
        },
    ],
};

pub(super) static BREEZE_SLIDE_BACK: KfAnim = KfAnim {
    length: 0.1,
    looping: false,
    channels: &[
        KfChannel {
            part: "body",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, -6.0, Kfi::Lin),
                kf(0.1, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_mid",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, -3.0, Kfi::Lin),
                kf(0.1, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
        KfChannel {
            part: "wind_top",
            target: KfTarget::Pos,
            frames: &[
                kf(0.0, 0.0, 0.0, -2.0, Kfi::Lin),
                kf(0.1, 0.0, 0.0, 0.0, Kfi::Lin),
            ],
        },
    ],
};
