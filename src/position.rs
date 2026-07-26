//! Conversions between the three `Position` types in play.
//!
//! The IPC, capture and wire protocol layers each define their own screen-edge
//! enum. They are structurally identical but live in different crates, so no
//! blanket `From` impl is possible; these functions are the single place where
//! the mapping is defined.

pub(crate) fn ipc_to_capture(pos: lan_mouse_ipc::Position) -> input_capture::Position {
    match pos {
        lan_mouse_ipc::Position::Left => input_capture::Position::Left,
        lan_mouse_ipc::Position::Right => input_capture::Position::Right,
        lan_mouse_ipc::Position::Top => input_capture::Position::Top,
        lan_mouse_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
}

pub(crate) fn capture_to_proto(pos: input_capture::Position) -> lan_mouse_proto::Position {
    match pos {
        input_capture::Position::Left => lan_mouse_proto::Position::Left,
        input_capture::Position::Right => lan_mouse_proto::Position::Right,
        input_capture::Position::Top => lan_mouse_proto::Position::Top,
        input_capture::Position::Bottom => lan_mouse_proto::Position::Bottom,
    }
}

pub(crate) fn proto_to_ipc(pos: lan_mouse_proto::Position) -> lan_mouse_ipc::Position {
    match pos {
        lan_mouse_proto::Position::Left => lan_mouse_ipc::Position::Left,
        lan_mouse_proto::Position::Right => lan_mouse_ipc::Position::Right,
        lan_mouse_proto::Position::Top => lan_mouse_ipc::Position::Top,
        lan_mouse_proto::Position::Bottom => lan_mouse_ipc::Position::Bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_conversions_round_trip() {
        for (ipc, capture, proto) in [
            (
                lan_mouse_ipc::Position::Left,
                input_capture::Position::Left,
                lan_mouse_proto::Position::Left,
            ),
            (
                lan_mouse_ipc::Position::Right,
                input_capture::Position::Right,
                lan_mouse_proto::Position::Right,
            ),
            (
                lan_mouse_ipc::Position::Top,
                input_capture::Position::Top,
                lan_mouse_proto::Position::Top,
            ),
            (
                lan_mouse_ipc::Position::Bottom,
                input_capture::Position::Bottom,
                lan_mouse_proto::Position::Bottom,
            ),
        ] {
            assert_eq!(ipc_to_capture(ipc), capture);
            assert_eq!(capture_to_proto(capture), proto);
            assert_eq!(proto_to_ipc(proto), ipc);
        }
    }
}
