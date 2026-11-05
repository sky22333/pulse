use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    },
    UI::{
        Input::{
            GetRawInputData, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK,
            RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
        },
        WindowsAndMessaging::*,
    },
};

const LIMIT: usize = 1_000_000;
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Keyboard,
    Mouse,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Sample {
    pub ticks: i64,
    pub code: u16,
    pub flags: u16,
    pub x: i32,
    pub y: i32,
}
#[derive(Serialize, Deserialize)]
pub struct Recording {
    pub kind: Kind,
    pub frequency: i64,
    pub device: usize,
    pub samples: Vec<Sample>,
    pub dropped: usize,
    pub errors: usize,
}
thread_local! { static STATE: RefCell<Option<Recording>> = const { RefCell::new(None) }; }
thread_local! { static FIRST_EVENT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) }; }

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        let mut ticks = 0;
        unsafe {
            QueryPerformanceCounter(&mut ticks);
        }
        let mut raw: RAWINPUT = unsafe { zeroed() };
        let mut len = size_of::<RAWINPUT>() as u32;
        let got = unsafe {
            GetRawInputData(
                lp as _,
                RID_INPUT,
                (&mut raw as *mut RAWINPUT).cast(),
                &mut len,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        STATE.with_borrow_mut(|slot| {
            if let Some(rec) = slot {
                if got == u32::MAX {
                    rec.errors += 1;
                    return;
                }
                let target = match rec.kind {
                    Kind::Keyboard => RIM_TYPEKEYBOARD,
                    Kind::Mouse => RIM_TYPEMOUSE,
                };
                if raw.header.dwType != target || raw.header.hDevice.is_null() {
                    return;
                }
                let sample = if rec.kind == Kind::Keyboard {
                    let k = unsafe { raw.data.keyboard };
                    Sample {
                        ticks,
                        code: k.MakeCode,
                        flags: k.Flags,
                        x: 0,
                        y: 0,
                    }
                } else {
                    let m = unsafe { raw.data.mouse };
                    if m.lLastX == 0 && m.lLastY == 0 {
                        return;
                    }
                    Sample {
                        ticks,
                        code: 0,
                        flags: m.usFlags,
                        x: m.lLastX,
                        y: m.lLastY,
                    }
                };
                let device = raw.header.hDevice as usize;
                if rec.device == 0 {
                    rec.device = device;
                }
                if device != rec.device {
                    return;
                }
                if rec.samples.len() < LIMIT {
                    rec.samples.push(sample);
                    if rec.samples.len() == 1 {
                        FIRST_EVENT.with_borrow(|flag| {
                            if let Some(flag) = flag {
                                flag.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                } else {
                    rec.dropped += 1;
                }
            }
        });
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    if msg == WM_CLOSE {
        unsafe {
            DestroyWindow(hwnd);
        }
        return 0;
    }
    if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub struct Capture {
    hwnd: isize,
    handle: JoinHandle<Recording>,
    first_event: Arc<AtomicBool>,
}
impl Capture {
    pub fn start(kind: Kind) -> Result<Self, String> {
        let (tx, rx) = mpsc::sync_channel::<Result<isize, String>>(1);
        let first_event = Arc::new(AtomicBool::new(false));
        let thread_flag = first_event.clone();
        let handle = thread::spawn(move || {
            FIRST_EVENT.with_borrow_mut(|flag| *flag = Some(thread_flag));
            let mut frequency = 0;
            unsafe {
                QueryPerformanceFrequency(&mut frequency);
            }
            STATE.with_borrow_mut(|s| {
                *s = Some(Recording {
                    kind,
                    frequency,
                    device: 0,
                    samples: Vec::with_capacity(100_000),
                    dropped: 0,
                    errors: 0,
                })
            });
            unsafe {
                let instance = GetModuleHandleW(null());
                let class: Vec<u16> = "PulseRawInput\0".encode_utf16().collect();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..zeroed()
                };
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    null_mut(),
                    instance,
                    null(),
                );
                if hwnd.is_null() {
                    let _ = tx.send(Err("无法创建输入采集窗口".into()));
                } else {
                    let dev = RAWINPUTDEVICE {
                        usUsagePage: 1,
                        usUsage: if kind == Kind::Keyboard { 6 } else { 2 },
                        dwFlags: RIDEV_INPUTSINK,
                        hwndTarget: hwnd,
                    };
                    if RegisterRawInputDevices(&dev, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
                        let _ = tx.send(Err("无法注册 Windows 原始输入".into()));
                        DestroyWindow(hwnd);
                    } else {
                        let _ = tx.send(Ok(hwnd as isize));
                        let mut msg: MSG = zeroed();
                        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
            }
            STATE.with_borrow_mut(|s| s.take().unwrap())
        });
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                handle,
                first_event,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn first_event(&self) -> Arc<AtomicBool> {
        self.first_event.clone()
    }
    pub fn stop(self) -> Result<Recording, String> {
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        self.handle.join().map_err(|_| "采集线程异常结束".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_can_start_and_stop() {
        let session = Capture::start(Kind::Keyboard).expect("Raw Input should register");
        let recording = session.stop().expect("capture thread should exit cleanly");
        assert!(recording.frequency > 0);
    }
}
