//! Colors of the UI as named tokens, in a dark and a light theme. Widgets only ever use
//! these names, so a theme is one table.

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    /// Behind the panels.
    pub window: u32,
    pub panel: u32,
    /// Section headers, the menu bar and the status bar.
    pub bar: u32,
    pub separator: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_disabled: u32,
    /// A key that is not set: its default, shown in place of a value.
    pub text_default: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub widget: u32,
    pub widget_hover: u32,
    pub widget_active: u32,
    pub border: u32,
    pub field: u32,
    pub focus: u32,
    pub text_selection: u32,
    pub row_hover: u32,
    pub row_selected: u32,
    pub popup: u32,
    pub tooltip: u32,
    pub tooltip_text: u32,
    pub error: u32,
    pub warning: u32,
    /// Axis labels of vector fields (x, y, z, w).
    pub axes: [u32; 4],
    pub scrollbar: u32,
    pub drop_target: u32,
    /// Opacity of the layer that dims the window behind a modal dialog (0-255).
    pub modal_dim: u8,
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            name: "dark",
            window: 0x18_18_1b,
            panel: 0x24_25_29,
            bar: 0x1d_1e_21,
            separator: 0x13_13_15,
            text: 0xdc_dc_e0,
            text_dim: 0x9a_9b_a3,
            text_disabled: 0x62_63_6a,
            text_default: 0x80_81_8a,
            accent: 0x4a_7f_d9,
            accent_text: 0xff_ff_ff,
            widget: 0x34_35_3b,
            widget_hover: 0x40_42_49,
            widget_active: 0x4d_50_58,
            border: 0x15_15_18,
            field: 0x1b_1c_1f,
            focus: 0x5d_8f_e6,
            text_selection: 0x2f_4f_86,
            row_hover: 0x2f_30_35,
            row_selected: 0x33_4a_72,
            popup: 0x2b_2c_31,
            tooltip: 0x3a_3b_42,
            tooltip_text: 0xe8_e8_ec,
            error: 0xf0_6a_6a,
            warning: 0xe6_b4_50,
            axes: [0xd8_5a_5a, 0x6a_bb_5a, 0x5a_8a_e0, 0xb0_b0_b8],
            scrollbar: 0x4a_4c_53,
            drop_target: 0x6e_a0_f0,
            modal_dim: 120,
        }
    }

    pub fn light() -> Theme {
        Theme {
            name: "light",
            window: 0xc8_ca_cf,
            panel: 0xee_ef_f2,
            bar: 0xe0_e2_e6,
            separator: 0xbe_c0_c6,
            text: 0x1e_1f_23,
            text_dim: 0x5c_5e_66,
            text_disabled: 0xa0_a2_aa,
            text_default: 0x86_88_90,
            accent: 0x2f_6b_d0,
            accent_text: 0xff_ff_ff,
            widget: 0xdb_dd_e2,
            widget_hover: 0xcf_d2_d9,
            widget_active: 0xc0_c4_cc,
            border: 0xb4_b7_bf,
            field: 0xff_ff_ff,
            focus: 0x2f_6b_d0,
            text_selection: 0xb5_cd_f5,
            row_hover: 0xe2_e5_ea,
            row_selected: 0xc5_d7_f5,
            popup: 0xf8_f8_fa,
            tooltip: 0xff_fb_e6,
            tooltip_text: 0x2a_2a_2a,
            error: 0xc8_30_30,
            warning: 0xa8_6a_00,
            axes: [0xc0_38_38, 0x3a_8a_2a, 0x30_60_c8, 0x70_70_78],
            scrollbar: 0xb0_b3_ba,
            drop_target: 0x2f_6b_d0,
            modal_dim: 90,
        }
    }

    pub fn named(name: &str) -> Theme {
        if name == "light" { Theme::light() } else { Theme::dark() }
    }
}
