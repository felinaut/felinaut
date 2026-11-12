//! Provenance of the image that actually supplies Cocoa's WKWebView class.
//!
//! No filesystem inspection or path-based classification: image names are only
//! compared in memory, and only the source and mapped image UUID leave this module.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use super::super::{RuntimeIdentity, RuntimeSource};

const HEADER_SIZE: usize = 32; // mach_header_64
const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_UUID: u32 = 0x1b;
const UUID_COMMAND_SIZE: usize = 24;
// A conservative ceiling before forming a slice over loader-owned memory.
const MAX_COMMAND_BYTES: usize = 16 * 1024 * 1024;

#[repr(C)]
struct DlInfo {
    name: *const c_char,
    header: *const c_void,
    symbol_name: *const c_char,
    symbol_address: *const c_void,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_lookUpClass(name: *const c_char) -> *const c_void;
    fn class_getImageName(class: *const c_void) -> *const c_char;
}

unsafe extern "C" {
    fn pthread_main_np() -> c_int;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

pub(super) fn identity() -> RuntimeIdentity {
    // The browser calls this on its UI thread. Refuse off-thread callers rather
    // than extending that ownership assumption to arbitrary diagnostic threads.
    // SAFETY: pthread_main_np has no preconditions or borrowed output.
    if unsafe { pthread_main_np() } == 0 {
        return RuntimeIdentity::default();
    }
    // SAFETY: On the UI thread, WKWebView's linked framework remains loaded for
    // the process lifetime. ObjC/dyld strings and mapped headers are borrowed
    // only during this call. No callbacks, dlopen/dlclose, or file reads occur.
    unsafe { loaded_identity() }.unwrap_or_default()
}

/// Caller must keep the WKWebView image loaded throughout all native borrows.
unsafe fn loaded_identity() -> Option<RuntimeIdentity> {
    // SAFETY: A constant NUL-terminated name; lookup does not instantiate a view.
    let class = unsafe { objc_lookUpClass(c"WKWebView".as_ptr()) };
    if class.is_null() {
        return None;
    }
    // SAFETY: The class is registered and its image remains loaded.
    let class_image = unsafe { class_getImageName(class) };
    if class_image.is_null() {
        return None;
    }
    let mut image = DlInfo {
        name: ptr::null(),
        header: ptr::null(),
        symbol_name: ptr::null(),
        symbol_address: ptr::null(),
    };
    // SAFETY: A live class address and writable Dl_info with the SDK's C layout.
    // dyld.h recommends dladdr for thread-safe loaded-image header/name lookup
    // instead of racing _dyld_image_count/_dyld_get_image_* enumeration.
    if unsafe { dladdr(class, &mut image) } == 0 || image.name.is_null() || image.header.is_null() {
        return None;
    }
    // SAFETY: ObjC and dyld return NUL-terminated strings owned by the still-loaded
    // class image. An alias or inconsistent lookup is unverified, never guessed.
    if unsafe { CStr::from_ptr(class_image) != CStr::from_ptr(image.name) } {
        return None;
    }
    // SAFETY: dladdr supplied this mapped image header, pinned by the class image.
    let uuid = unsafe { mapped_uuid(image.header.cast()) }?;
    // SAFETY: Queries dyld's own range; no image or environment path is consulted.
    let (cache_start, cache_size) = unsafe { shared_cache_range() }.unwrap_or((0, 0));
    Some(RuntimeIdentity {
        source: source_for_range(image.header as usize, cache_start, cache_size),
        image_uuid: Some(format_uuid(uuid)),
    })
}

unsafe fn shared_cache_range() -> Option<(usize, usize)> {
    type GetRange = unsafe extern "C" fn(*mut usize) -> *const c_void;
    // The SDK's libdyld exports this symbol, though dyld.h does not declare it.
    // Resolve optionally so an unavailable query means Unverified, not a launch
    // failure or an assumption that an image is external to the system cache.
    const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;
    // SAFETY: RTLD_DEFAULT and the symbol name follow the macOS dlfcn.h ABI.
    let symbol = unsafe { dlsym(RTLD_DEFAULT, c"_dyld_get_shared_cache_range".as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: dyld's exported function has signature const void *(size_t *).
    // The libSystem/dyld implementation stays loaded for the process lifetime.
    let get_range: GetRange = unsafe { std::mem::transmute(symbol) };
    let mut size = 0;
    // SAFETY: Writable size_t output; the returned address is never dereferenced.
    let start = unsafe { get_range(&mut size) };
    Some((start as usize, size))
}

fn source_for_range(header: usize, cache_start: usize, cache_size: usize) -> RuntimeSource {
    let Some(cache_end) = cache_start.checked_add(cache_size) else {
        return RuntimeSource::Unverified;
    };
    if header == 0 || cache_start == 0 || cache_size == 0 {
        RuntimeSource::Unverified
    } else if (cache_start..cache_end).contains(&header) {
        RuntimeSource::SystemSharedCache
    } else {
        RuntimeSource::ExternalFramework
    }
}

/// Read only a loader-validated, still-loaded 64-bit Mach-O image header.
unsafe fn mapped_uuid(header: *const u8) -> Option<[u8; 16]> {
    (header as usize).checked_add(HEADER_SIZE)?;
    // SAFETY: dladdr's non-null mapped header contains mach_header_64 on supported
    // macOS architectures (arm64/x86_64). dyld validated the image when loading it.
    let prefix = unsafe { std::slice::from_raw_parts(header, HEADER_SIZE) };
    let (_, commands_size) = command_layout(prefix)?;
    let length = HEADER_SIZE.checked_add(commands_size)?;
    (header as usize).checked_add(length)?;
    // SAFETY: dyld validates and maps the load-command region as part of loading
    // the image. command_layout caps size/count and validates the header before
    // extending the borrow. The safe parser checks every command's bounds too.
    let image = unsafe { std::slice::from_raw_parts(header, length) };
    parse_uuid(image)
}

fn word(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn command_layout(image: &[u8]) -> Option<(usize, usize)> {
    if image.len() < HEADER_SIZE || word(image, 0)? != MH_MAGIC_64 {
        return None;
    }
    let count = word(image, 16)? as usize;
    let size = word(image, 20)? as usize;
    if size > MAX_COMMAND_BYTES || !size.is_multiple_of(8) || count > size / 8 {
        return None;
    }
    Some((count, size))
}

/// Safe parser for a thin, little-endian 64-bit Mach-O header/load-command slice.
/// Both currently supported macOS architectures use this layout. Other formats,
/// duplicate UUIDs, and malformed commands are deliberately not identified.
fn parse_uuid(image: &[u8]) -> Option<[u8; 16]> {
    let (count, size) = command_layout(image)?;
    let commands = image.get(HEADER_SIZE..HEADER_SIZE.checked_add(size)?)?;
    let mut offset: usize = 0;
    let mut uuid = None;
    for _ in 0..count {
        let command = word(commands, offset)?;
        let length = word(commands, offset.checked_add(4)?)? as usize;
        if length < 8 || !length.is_multiple_of(8) {
            return None;
        }
        let end = offset.checked_add(length)?;
        let payload = commands.get(offset..end)?;
        if command == LC_UUID {
            if length != UUID_COMMAND_SIZE || uuid.is_some() {
                return None;
            }
            uuid = Some(payload.get(8..24)?.try_into().ok()?);
        }
        offset = end;
    }
    // Validate the entire declared region, including commands following LC_UUID.
    (offset == commands.len()).then_some(uuid).flatten()
}

fn format_uuid(uuid: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(36);
    for (index, byte) in uuid.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            result.push('-');
        }
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: [u8; 16] = [
        0x00, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
        0xff,
    ];

    fn set_word(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn image(commands: &[&[u8]]) -> Vec<u8> {
        let mut image = vec![0; HEADER_SIZE];
        set_word(&mut image, 0, MH_MAGIC_64);
        set_word(&mut image, 16, commands.len() as u32);
        set_word(
            &mut image,
            20,
            commands.iter().map(|command| command.len() as u32).sum(),
        );
        for command in commands {
            image.extend_from_slice(command);
        }
        image
    }

    fn uuid_command() -> Vec<u8> {
        let mut command = vec![0; UUID_COMMAND_SIZE];
        set_word(&mut command, 0, LC_UUID);
        set_word(&mut command, 4, UUID_COMMAND_SIZE as u32);
        command[8..].copy_from_slice(&UUID);
        command
    }

    #[test]
    fn reads_uuid_among_other_commands_and_formats_without_paths() {
        let other = [0x42, 0, 0, 0, 8, 0, 0, 0];
        let bytes = image(&[&other, &uuid_command(), &other]);
        assert_eq!(parse_uuid(&bytes), Some(UUID));
        assert_eq!(format_uuid(UUID), "00123456-789A-BCDE-F012-3456789ABCFF");
        // A whole mapped image may contain data beyond the load-command region.
        let mut with_data = bytes;
        with_data.extend_from_slice(&[0xff; 64]);
        assert_eq!(parse_uuid(&with_data), Some(UUID));
    }

    #[test]
    fn empty_or_missing_uuid_is_unverified() {
        assert_eq!(parse_uuid(&[]), None);
        assert_eq!(parse_uuid(&image(&[])), None);
        assert_eq!(parse_uuid(&image(&[&[0x42, 0, 0, 0, 8, 0, 0, 0]])), None);
    }

    #[test]
    fn every_truncation_of_a_uuid_image_is_rejected() {
        let bytes = image(&[&uuid_command()]);
        for length in 0..bytes.len() {
            assert_eq!(parse_uuid(&bytes[..length]), None, "length {length}");
        }
        assert_eq!(parse_uuid(&bytes), Some(UUID));
    }

    #[test]
    fn rejects_non_native_magic_and_unbounded_header_fields() {
        let valid = image(&[&uuid_command()]);
        for magic in [0, 0xfeed_face, 0xcffa_edfe, 0xcafe_babe] {
            let mut bytes = valid.clone();
            set_word(&mut bytes, 0, magic);
            assert_eq!(parse_uuid(&bytes), None);
        }
        for (field, value) in [
            (16, 0),
            (16, 4),
            (16, u32::MAX),
            (20, 0),
            (20, 23),
            (20, 32),
            (20, MAX_COMMAND_BYTES as u32 + 8),
            (20, u32::MAX),
        ] {
            let mut bytes = valid.clone();
            set_word(&mut bytes, field, value);
            assert_eq!(parse_uuid(&bytes), None, "field {field}, value {value}");
        }
        let mut prefix = valid[..HEADER_SIZE].to_vec();
        set_word(&mut prefix, 16, (MAX_COMMAND_BYTES / 8) as u32);
        set_word(&mut prefix, 20, MAX_COMMAND_BYTES as u32);
        assert_eq!(
            command_layout(&prefix),
            Some((MAX_COMMAND_BYTES / 8, MAX_COMMAND_BYTES))
        );
        assert_eq!(parse_uuid(&prefix), None);
    }

    #[test]
    fn rejects_invalid_command_sizes_duplicate_uuids_and_malformed_tail() {
        for size in [0, 4, 8, 16, 23, 32, u32::MAX] {
            let mut command = uuid_command();
            set_word(&mut command, 4, size);
            assert_eq!(parse_uuid(&image(&[&command])), None, "size {size}");
        }
        let mut oversized_uuid = uuid_command();
        oversized_uuid.extend_from_slice(&[0; 8]);
        set_word(&mut oversized_uuid, 4, 32);
        assert_eq!(parse_uuid(&image(&[&oversized_uuid])), None);
        assert_eq!(
            parse_uuid(&image(&[&uuid_command(), &uuid_command()])),
            None
        );
        for tail in [
            [0x42, 0, 0, 0, 0, 0, 0, 0],
            [0x42, 0, 0, 0, 7, 0, 0, 0],
            [0x42, 0, 0, 0, 16, 0, 0, 0],
        ] {
            assert_eq!(parse_uuid(&image(&[&uuid_command(), &tail])), None);
        }
    }

    #[test]
    fn source_uses_only_a_valid_half_open_cache_address_range() {
        assert_eq!(
            source_for_range(100, 100, 100),
            RuntimeSource::SystemSharedCache
        );
        assert_eq!(
            source_for_range(199, 100, 100),
            RuntimeSource::SystemSharedCache
        );
        assert_eq!(
            source_for_range(99, 100, 100),
            RuntimeSource::ExternalFramework
        );
        assert_eq!(
            source_for_range(200, 100, 100),
            RuntimeSource::ExternalFramework
        );
        for (header, start, size) in [
            (0, 100, 100),
            (100, 0, 100),
            (100, 100, 0),
            (100, usize::MAX, 1),
            (100, 100, usize::MAX),
        ] {
            assert_eq!(
                source_for_range(header, start, size),
                RuntimeSource::Unverified
            );
        }
    }

    #[test]
    fn off_ui_thread_queries_fail_closed() {
        let identity = std::thread::spawn(identity).join().unwrap();
        assert_eq!(identity.source, RuntimeSource::Unverified);
        assert!(identity.image_uuid.is_none());
    }
}
