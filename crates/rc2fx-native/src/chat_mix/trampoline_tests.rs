//! Execute each real displaced prologue and the Rust hook ABI under AArch64.
use super::*;

std::arch::global_asm!(
    r#"
.text
.balign 4096
.global rc2fx_chat_fixture_mixer
.type rc2fx_chat_fixture_mixer,%function
rc2fx_chat_fixture_mixer:
.inst 0xd503233f,0xd12983ff,0xa9007bfd,0x910003fd
stp x0,x1,[x0]
stp x2,x3,[x0,#16]
stp x4,x5,[x0,#32]
stp x6,x7,[x0,#48]
ldp x29,x30,[sp]
add sp,sp,#0xa60
.inst 0xd50323bf
ret
.balign 4096
.global rc2fx_chat_fixture_stream
.type rc2fx_chat_fixture_stream,%function
rc2fx_chat_fixture_stream:
.inst 0xd503233f,0xa9b87bfd,0x910003fd,0xa9025bf5
str w1,[x0,#8]
ldp x21,x22,[sp,#32]
ldp x29,x30,[sp]
add sp,sp,#0x80
.inst 0xd50323bf
ret
.balign 4096
.global rc2fx_chat_fixture_sum
.type rc2fx_chat_fixture_sum,%function
rc2fx_chat_fixture_sum:
.inst 0xd503233f,0xa9b77bfd,0x910003fd,0xa90153f3
stp x0,x1,[x0]
ldp x19,x20,[sp,#16]
ldp x29,x30,[sp]
add sp,sp,#0x90
.inst 0xd50323bf
ret
.balign 4096
.global rc2fx_chat_fixture_chat
.type rc2fx_chat_fixture_chat,%function
rc2fx_chat_fixture_chat:
.inst 0xd503233f,0xa9b97bfd,0x910003fd,0x39540803
stp x1,x2,[x0,#8]
ldp x29,x30,[sp]
add sp,sp,#0x70
.inst 0xd50323bf
ret
.balign 4096
"#
);

unsafe extern "C-unwind" {
    fn rc2fx_chat_fixture_mixer(
        a: usize,
        b: usize,
        c: i32,
        d: usize,
        e: i32,
        f: u32,
        g: i64,
        h: i64,
    );
    fn rc2fx_chat_fixture_stream(a: usize, b: i32);
    fn rc2fx_chat_fixture_sum(a: usize, b: usize);
    fn rc2fx_chat_fixture_chat(a: usize, b: usize, c: usize);
}
#[test]
fn all_four_detours_preserve_register_arguments_stack_and_return() {
    disarm();
    let entries = [
        rc2fx_chat_fixture_mixer as *const () as usize,
        rc2fx_chat_fixture_stream as *const () as usize,
        rc2fx_chat_fixture_sum as *const () as usize,
        rc2fx_chat_fixture_chat as *const () as usize,
    ];
    for ((site, (hook, original)), words) in entries
        .into_iter()
        .zip([
            (mixer_hook as *const () as usize, &ORIGINAL_MIXER),
            (stream_hook as *const () as usize, &ORIGINAL_STREAM),
            (sum_hook as *const () as usize, &ORIGINAL_SUM),
            (chat_hook as *const () as usize, &ORIGINAL_CHAT),
        ])
        .zip(PROLOGUES)
    {
        let expected: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        assert_eq!(
            unsafe { std::slice::from_raw_parts(site as *const u8, 16) },
            expected
        );
        unsafe {
            crate::vendor_job::install_detour(site as *mut u8, hook, original).unwrap();
        }
    }
    let mut storage = [0usize; 512];
    let address = storage.as_mut_ptr() as usize;
    unsafe {
        rc2fx_chat_fixture_mixer(address, 0x11, 2, 0x33, 112, 48_000, 0x66, 0x77);
    }
    assert_eq!(
        &storage[..8],
        &[address, 0x11, 2, 0x33, 112, 48_000, 0x66, 0x77]
    );
    unsafe {
        rc2fx_chat_fixture_stream(address, 112);
    }
    assert_eq!(storage[1], 112);
    unsafe {
        rc2fx_chat_fixture_sum(address, 0x1234);
    }
    assert_eq!(&storage[..2], &[address, 0x1234]);
    unsafe {
        rc2fx_chat_fixture_chat(address, 0x2345, 0x3456);
    }
    assert_eq!(&storage[1..3], &[0x2345, 0x3456]);
}
