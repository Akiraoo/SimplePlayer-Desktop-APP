//! macOS exclusive mode ("hog mode"): while Simple Player holds the output device, no other
//! program can play through it — the macOS counterpart of WASAPI exclusive mode.
//!
//! CoreAudio's `kAudioDevicePropertyHogMode` holds the pid of the process that owns the
//! device (-1 = shared). Setting it toggles: it takes the device when it is free and gives
//! it back when we own it. macOS also gives it back by itself if the process ends.

use std::ffi::{c_char, c_void};

type OsStatus = i32;
type ObjectId = u32;

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const SYSTEM_OBJECT: ObjectId = 1;
const DEVICES: u32 = fourcc(b"dev#");
const DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const NAME: u32 = fourcc(b"lnam");
const HOG_MODE: u32 = fourcc(b"oink");
const STREAMS: u32 = fourcc(b"stm#");
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");
const UTF8: u32 = 0x0800_0100;

#[cfg_attr(target_os = "macos", link(name = "CoreAudio", kind = "framework"))]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> OsStatus;
    fn AudioObjectGetPropertyData(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> OsStatus;
    fn AudioObjectSetPropertyData(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OsStatus;
}

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
extern "C" {
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

fn address(selector: u32, scope: u32) -> Address {
    Address {
        selector,
        scope,
        element: 0, // kAudioObjectPropertyElementMain
    }
}

/// Reads a fixed-size property.
fn get<T: Copy>(id: ObjectId, selector: u32, scope: u32, init: T) -> Option<T> {
    let mut value = init;
    let mut size = std::mem::size_of::<T>() as u32;
    let a = address(selector, scope);
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut T as *mut c_void,
        )
    };
    (status == 0).then_some(value)
}

fn data_size(id: ObjectId, selector: u32, scope: u32) -> u32 {
    let mut size = 0u32;
    let a = address(selector, scope);
    let status =
        unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, std::ptr::null(), &mut size) };
    if status == 0 {
        size
    } else {
        0
    }
}

fn device_ids() -> Vec<ObjectId> {
    let size = data_size(SYSTEM_OBJECT, DEVICES, SCOPE_GLOBAL);
    let count = size as usize / std::mem::size_of::<ObjectId>();
    let mut ids = vec![0 as ObjectId; count];
    let mut size = size;
    let a = address(DEVICES, SCOPE_GLOBAL);
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / std::mem::size_of::<ObjectId>());
    ids
}

/// The device's name, the same one cpal reports.
fn device_name(id: ObjectId) -> Option<String> {
    let cf: *const c_void = get(id, NAME, SCOPE_GLOBAL, std::ptr::null())?;
    if cf.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    let ok = unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, UTF8) };
    unsafe { CFRelease(cf) };
    if ok == 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

fn is_output(id: ObjectId) -> bool {
    data_size(id, STREAMS, SCOPE_OUTPUT) > 0
}

/// The output device called `name`, or the system default output.
pub fn find(name: Option<&str>) -> Option<u32> {
    match name {
        Some(n) => device_ids()
            .into_iter()
            .find(|&id| is_output(id) && device_name(id).as_deref() == Some(n)),
        None => get(SYSTEM_OBJECT, DEFAULT_OUTPUT, SCOPE_GLOBAL, 0 as ObjectId).filter(|&id| id != 0),
    }
}

fn owner(id: ObjectId) -> Option<i32> {
    get(id, HOG_MODE, SCOPE_GLOBAL, -1i32)
}

fn toggle(id: ObjectId) -> bool {
    let mut value = std::process::id() as i32;
    let a = address(HOG_MODE, SCOPE_GLOBAL);
    let status = unsafe {
        AudioObjectSetPropertyData(
            id,
            &a,
            0,
            std::ptr::null(),
            std::mem::size_of::<i32>() as u32,
            &mut value as *mut i32 as *const c_void,
        )
    };
    status == 0
}

/// Exclusive access to one device, given back when dropped.
pub struct Hog {
    id: ObjectId,
}

impl Hog {
    pub fn take(id: u32) -> Result<Hog, String> {
        let me = std::process::id() as i32;
        match owner(id) {
            Some(pid) if pid == me => return Ok(Hog { id }),
            Some(-1) => {}
            Some(pid) => {
                return Err(crate::tr!(
                    "裝置正被另一個程式獨佔（pid {}）",
                    "Another program has the device to itself (pid {})",
                    pid
                ))
            }
            None => {
                return Err(crate::tr!("這個裝置不支援獨佔", "This device can't be used exclusively"))
            }
        }
        if toggle(id) && owner(id) == Some(me) {
            Ok(Hog { id })
        } else {
            Err(crate::tr!("無法獨佔這個裝置", "Couldn't take the device exclusively"))
        }
    }

    pub fn device(&self) -> u32 {
        self.id
    }
}

impl Drop for Hog {
    fn drop(&mut self) {
        if owner(self.id) == Some(std::process::id() as i32) {
            toggle(self.id);
        }
    }
}
