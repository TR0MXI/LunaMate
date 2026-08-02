use windows::Win32::Security::PSID;

use super::super::windows_security::construct_private_acl_for_test;

#[test]
fn private_acl_construction_accepts_only_the_supplied_sid_policy() {
    // S-1-5-21，使用 `u32` 数组保证传给 Win32 的 SID 至少四字节对齐。
    let mut sid_words = [0x0000_0101_u32, 0x0500_0000, 21];
    let sid = PSID(sid_words.as_mut_ptr().cast());

    construct_private_acl_for_test(sid, false).expect("文件保护 DACL 应可从有效 SID 构造");
    construct_private_acl_for_test(sid, true).expect("目录保护 DACL 应包含继承标志");
}
