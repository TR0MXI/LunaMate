//! 集中声明窗口适配所需的最小 XCB 原生接口。

use std::ffi::c_void;

#[repr(C)]
pub(super) struct XcbVoidCookie {
    sequence: u32,
}

#[repr(C)]
pub(super) struct XcbQueryPointerCookie {
    sequence: u32,
}

#[repr(C)]
pub(super) struct XcbQueryPointerReply {
    response_type: u8,
    pub(super) same_screen: u8,
    sequence: u16,
    length: u32,
    root: u32,
    child: u32,
    root_x: i16,
    root_y: i16,
    pub(super) win_x: i16,
    pub(super) win_y: i16,
    mask: u16,
    padding: [u8; 2],
}

#[link(name = "xcb")]
unsafe extern "C" {
    pub(super) fn xcb_map_window(connection: *mut c_void, window: u32) -> XcbVoidCookie;
    pub(super) fn xcb_unmap_window(connection: *mut c_void, window: u32) -> XcbVoidCookie;
    pub(super) fn xcb_query_pointer(connection: *mut c_void, window: u32) -> XcbQueryPointerCookie;
    pub(super) fn xcb_query_pointer_reply(
        connection: *mut c_void,
        cookie: XcbQueryPointerCookie,
        error: *mut *mut c_void,
    ) -> *mut XcbQueryPointerReply;
    pub(super) fn xcb_configure_window(
        connection: *mut c_void,
        window: u32,
        value_mask: u16,
        value_list: *const c_void,
    ) -> XcbVoidCookie;
    pub(super) fn xcb_flush(connection: *mut c_void) -> i32;
}

unsafe extern "C" {
    pub(super) fn free(pointer: *mut c_void);
}
