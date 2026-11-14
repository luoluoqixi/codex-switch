//! Native listener ownership checks. No shell process or theme data is involved.
#![cfg(target_os = "windows")]

use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs,
    mem::{offset_of, size_of},
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
            MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
    core::PWSTR,
};

/// An absent listener is normal while the user has not restarted Codex yet.
/// Any listener that can answer the client's IPv4 loopback connection must
/// belong to the selected executable, or to a conservatively identified Codex.
pub(super) fn verify_listener(port: u16, expected: Option<&Path>) -> Result<()> {
    let mut owners = Vec::new();
    let table = tcp_table(AF_INET.0.into())?;
    for row in
        table_rows::<MIB_TCPROW_OWNER_PID>(&table, offset_of!(MIB_TCPTABLE_OWNER_PID, table))?
    {
        if u16::from_be(row.dwLocalPort as u16) != port {
            continue;
        }
        let address = row.dwLocalAddr.to_ne_bytes();
        if address == [0, 0, 0, 0] {
            bail!(
                crate::tr!("identity.theme_debugging_port_is_bound_to_all_network_interfaces"; port = port)
            );
        }
        if address == [127, 0, 0, 1] {
            owners.push(row.dwOwningPid);
        }
    }
    // An IPv6 wildcard listener may also accept IPv4 connections. Do not let
    // dual-stack sockets evade the ownership boundary.
    let table = tcp_table(AF_INET6.0.into())?;
    for row in
        table_rows::<MIB_TCP6ROW_OWNER_PID>(&table, offset_of!(MIB_TCP6TABLE_OWNER_PID, table))?
    {
        if u16::from_be(row.dwLocalPort as u16) != port {
            continue;
        }
        if row.ucLocalAddr == [0; 16] {
            bail!(
                crate::tr!("identity.theme_debugging_port_is_bound_to_all_ipv6_network"; port = port)
            );
        }
        if row.ucLocalAddr == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 127, 0, 0, 1] {
            owners.push(row.dwOwningPid);
        }
    }
    owners.sort_unstable();
    owners.dedup();
    for owner in owners {
        let actual =
            executable(owner).with_context(|| crate::tr!("identity.could_not_verify_the_process_using_theme_debugging_port"; port = port))?;
        if let Some(expected) = expected {
            ensure!(
                same_path(&actual, expected)?,
                crate::tr!("identity.theme_debugging_port_is_in_use_by_another_app"; port = port)
            );
        } else {
            ensure!(
                looks_like_codex(&actual),
                crate::tr!("identity.theme_debugging_port_does_not_belong_to_a_recognized"; port = port)
            );
        }
    }
    Ok(())
}

fn tcp_table(family: u32) -> Result<Vec<u8>> {
    let mut size = 0;
    let initial = unsafe {
        GetExtendedTcpTable(
            None,
            &mut size,
            false,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    ensure!(
        initial == 0 || initial == 122,
        crate::tr!("identity.could_not_query_system_tcp_listeners"; arg0 = std::io::Error::from_raw_os_error(initial as i32))
    );
    for _ in 0..4 {
        let mut bytes = vec![0u8; size.max(4) as usize];
        let result = unsafe {
            GetExtendedTcpTable(
                Some(bytes.as_mut_ptr().cast()),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if result == 122 {
            continue;
        }
        ensure!(
            result == 0,
            crate::tr!("identity.could_not_read_system_tcp_listeners"; arg0 = std::io::Error::from_raw_os_error(result as i32))
        );
        ensure!(
            size as usize <= bytes.len(),
            crate::tr!("identity.invalid_system_tcp_listener_table_length")
        );
        bytes.truncate(size as usize);
        return Ok(bytes);
    }
    bail!(crate::tr!(
        "identity.the_system_tcp_listener_table_changed_too_quickly_try"
    ))
}

fn table_rows<T: Copy>(bytes: &[u8], offset: usize) -> Result<Vec<T>> {
    let count = u32::from_ne_bytes(
        bytes
            .get(..4)
            .context(crate::tr!(
                "identity.invalid_system_tcp_listener_table_header"
            ))?
            .try_into()
            .unwrap(),
    ) as usize;
    let end = count
        .checked_mul(size_of::<T>())
        .and_then(|n| n.checked_add(offset))
        .context(crate::tr!(
            "identity.system_tcp_listener_table_length_overflow"
        ))?;
    ensure!(
        end <= bytes.len(),
        crate::tr!("identity.incomplete_system_tcp_listener_table")
    );
    // The byte buffer need not be aligned to the Win32 structure. Copy each
    // bounded row with an unaligned read instead of creating invalid references.
    Ok((0..count)
        .map(|index| unsafe {
            bytes
                .as_ptr()
                .add(offset + index * size_of::<T>())
                .cast::<T>()
                .read_unaligned()
        })
        .collect())
}

struct ProcessHandle(HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn executable(pid: u32) -> Result<PathBuf> {
    let handle = ProcessHandle(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.context(
            crate::tr!("identity.could_not_read_the_process_using_the_port"),
        )?,
    );
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .context(crate::tr!(
        "identity.could_not_read_the_app_path_for_the_port"
    ))?;
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn same_path(actual: &Path, expected: &Path) -> Result<bool> {
    let actual = fs::canonicalize(actual).context(crate::tr!(
        "identity.could_not_verify_the_debugging_process_s_actual_path"
    ))?;
    let expected = fs::canonicalize(expected).context(crate::tr!(
        "identity.could_not_verify_the_selected_app_s_actual_path"
    ))?;
    Ok(actual
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected.to_string_lossy()))
}

fn looks_like_codex(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("ChatGPT.exe"))
        && (path
            .parent()
            .is_some_and(|dir| dir.join("resources/app.asar").is_file())
            || path.components().any(|part| {
                part.as_os_str()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("openai.codex_")
            }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn native_listener_table_rejects_this_tools_port_as_codex() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(
            verify_listener(port, None)
                .unwrap_err()
                .to_string()
                .contains(&crate::tr!("identity.theme_debugging_port_does_not_belong_to_a_recognized"; port = port))
        );
        // The same listener is accepted when its exact executable was identified
        // by the caller, exercising QueryFullProcessImageNameW and canonicalize.
        verify_listener(port, Some(&std::env::current_exe().unwrap())).unwrap();
    }

    #[test]
    fn identity_fallback_does_not_accept_an_executable_name_alone() {
        assert!(!looks_like_codex(Path::new(r"C:\random\ChatGPT.exe")));
        assert!(!looks_like_codex(Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1\app\other.exe"
        )));
        assert!(looks_like_codex(Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1\app\ChatGPT.exe"
        )));
    }
}
