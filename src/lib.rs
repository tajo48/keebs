pub mod classic;
pub mod deck;
pub mod diag;
pub mod events;
pub mod lizardguard;
pub mod manager;
pub mod osk_main;
pub mod osk_state;
pub mod render;
pub mod sc;
pub mod sdl3;
pub mod stop;
pub mod triton;
pub mod uinput;
pub mod usb;
pub mod vkb;
pub mod wl_im;

/// Client bindings for input-method-unstable-v2, generated inline by
/// wayland-scanner's `generate_client_code!` proc macro from the vendored
/// XML — the wayland-protocols crate (0.32) does not ship zv2 bindings.
/// (The include_bytes! pins rebuilds to XML changes, since proc macros
/// don't register the file as a dependency.)
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code
)]
pub mod wayland_im_protocol {
    const _XML: &[u8] = include_bytes!("../protocols/input-method-unstable-v2.xml");
    pub use wayland_client;
    pub use wayland_client::protocol::wl_seat;

    // The macro generates the proxies/enums but expects the wire-metadata
    // statics to be provided here (wayland-protocols builds them with
    // scanner's build-time C generator, which is gone in 0.31 — so write
    // them directly from the vendored XML).
    use wayland_client::backend::protocol::{AllowNull, ArgumentType, Interface, MessageDesc};

    /// Placeholder for args that carry no interface (the backend only uses
    /// these for diagnostics).
    #[allow(non_upper_case_globals)]
    static NO_INTERFACE: Interface = Interface {
        name: "",
        version: 0,
        requests: &[],
        events: &[],
        c_ptr: None,
    };

    #[allow(non_upper_case_globals)]
    pub static ZWP_INPUT_METHOD_MANAGER_V2_INTERFACE: Interface = Interface {
        name: "zwp_input_method_manager_v2",
        version: 1,
        requests: &[
            MessageDesc {
                name: "get_input_method",
                since: 1,
                signature: &[ArgumentType::NewId, ArgumentType::Object(AllowNull::No)],
                arg_interfaces: &[&ZWP_INPUT_METHOD_V2_INTERFACE, &NO_INTERFACE],
                child_interface: Some(&ZWP_INPUT_METHOD_V2_INTERFACE),
                is_destructor: false,
            },
            MessageDesc {
                name: "destroy",
                since: 1,
                signature: &[],
                arg_interfaces: &[],
                child_interface: None,
                is_destructor: true,
            },
        ],
        events: &[],
        c_ptr: None,
    };

    #[allow(non_upper_case_globals)]
    pub static ZWP_INPUT_METHOD_V2_INTERFACE: Interface = Interface {
        name: "zwp_input_method_v2",
        version: 1,
        // Requests are never sent by keebs (text injection rides on uinput);
        // only the destructor placeholder is declared.
        requests: &[MessageDesc {
            name: "destroy",
            since: 1,
            signature: &[],
            arg_interfaces: &[],
            child_interface: None,
            is_destructor: true,
        }],
        events: &[
            MessageDesc {
                name: "activate",
                since: 1,
                signature: &[],
                arg_interfaces: &[],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "deactivate",
                since: 1,
                signature: &[],
                arg_interfaces: &[],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "surrounding_text",
                since: 1,
                signature: &[
                    ArgumentType::Str(AllowNull::Yes),
                    ArgumentType::Uint,
                    ArgumentType::Uint,
                ],
                arg_interfaces: &[&NO_INTERFACE, &NO_INTERFACE, &NO_INTERFACE],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "text_change_cause",
                since: 1,
                signature: &[ArgumentType::Uint],
                arg_interfaces: &[&NO_INTERFACE],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "content_type",
                since: 1,
                signature: &[ArgumentType::Uint, ArgumentType::Uint],
                arg_interfaces: &[&NO_INTERFACE, &NO_INTERFACE],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "done",
                since: 1,
                signature: &[],
                arg_interfaces: &[],
                child_interface: None,
                is_destructor: false,
            },
            MessageDesc {
                name: "unavailable",
                since: 1,
                signature: &[],
                arg_interfaces: &[],
                child_interface: None,
                is_destructor: false,
            },
        ],
        c_ptr: None,
    };

    wayland_scanner::generate_client_code!("protocols/input-method-unstable-v2.xml");
}
