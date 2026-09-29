//! Client bindings for the COSMIC protocols cosmo binds,
//! generated here from the protocol XML with `wayland-scanner` (MIT).
//!
//! These replace the `cosmic-protocols` crate, whose Rust code is
//! GPL-3.0-only and would have made every binary linking `cosmo-focus` a
//! GPL work. The XML files in `protocols/` carry their own permissive
//! (HPND-style) notices, reproduced unchanged; that notice is what they're
//! used under.

// Generated code: the scanner's output isn't held to this crate's lints.
#![allow(
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case
)]
#![allow(unused_imports, missing_docs, clippy::all)]

pub mod workspace_v1 {
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-workspace-unstable-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocols/cosmic-workspace-unstable-v1.xml");
    }
}

pub mod toplevel_info_v1 {
    pub mod client {
        use super::super::workspace_v1::client::*;
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
        use wayland_protocols::ext::workspace::v1::client::*;

        pub mod __interfaces {
            use super::super::super::workspace_v1::client::__interfaces::*;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
            use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/cosmic-toplevel-info-unstable-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocols/cosmic-toplevel-info-unstable-v1.xml");
    }
}

pub mod toplevel_management_v1 {
    pub mod client {
        use super::super::toplevel_info_v1::client::*;
        use super::super::workspace_v1::client::*;
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::workspace::v1::client::*;

        pub mod __interfaces {
            use super::super::super::toplevel_info_v1::client::__interfaces::*;
            use super::super::super::workspace_v1::client::__interfaces::*;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
            wayland_scanner::generate_interfaces!(
                "protocols/cosmic-toplevel-management-unstable-v1.xml"
            );
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!(
            "protocols/cosmic-toplevel-management-unstable-v1.xml"
        );
    }
}
