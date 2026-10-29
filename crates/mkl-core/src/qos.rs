//! Network priority for session traffic. On Windows the socket joins a qWAVE flow of type
//! "control" (Microsoft's class for user input), so its packets carry DSCP 0x38. Wi-Fi drivers
//! and access points map that to the WMM voice queue, which goes ahead of ordinary traffic on a
//! busy channel. Best effort: if Windows refuses, the connection works as before.

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawSocket;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
    use windows_sys::Win32::NetworkManagement::QoS::{
        QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle, QOSTrafficTypeControl, QOS_NON_ADAPTIVE_FLOW, QOS_VERSION,
    };

    /// Keeps the socket's priority flow alive; dropping it closes the flow.
    pub struct Priority(HANDLE);

    // The handle is only passed to qWAVE calls, which are thread-safe.
    unsafe impl Send for Priority {}

    impl Drop for Priority {
        fn drop(&mut self) {
            unsafe {
                QOSCloseHandle(self.0);
            }
        }
    }

    pub fn prioritize(stream: &tokio::net::TcpStream) -> Option<Priority> {
        let version = QOS_VERSION { MajorVersion: 1, MinorVersion: 0 };
        let mut handle: HANDLE = null_mut();
        unsafe {
            if QOSCreateHandle(&version, &mut handle) == 0 {
                tracing::debug!(err = GetLastError(), "QoS unavailable");
                return None;
            }
            let priority = Priority(handle);
            let mut flow = 0u32;
            let socket = stream.as_raw_socket() as usize;
            if QOSAddSocketToFlow(handle, socket, null(), QOSTrafficTypeControl, QOS_NON_ADAPTIVE_FLOW, &mut flow) == 0 {
                tracing::debug!(err = GetLastError(), "could not give the connection network priority");
                return None;
            }
            Some(priority)
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct Priority;

    pub fn prioritize(_stream: &tokio::net::TcpStream) -> Option<Priority> {
        None
    }
}

pub use imp::prioritize;
