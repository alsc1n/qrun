//! HTTPS GET 和 POST, 用系统自带的 WinHTTP: 不加依赖, 自动走系统代理 (国内访问 GitHub 常要靠它), 自动跟随跳转.
//! 阻塞调用, 要放在后台线程里.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::core::{PCWSTR, w};

use super::wide;

/// download 被 progress 中止时返回的错误.
pub const CANCELED: &str = "已取消";

/// WinHTTP 句柄, 离开作用域时关闭.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, step: &str) -> Result<Self, String> {
        if raw.is_null() {
            return Err(format!("{step}失败: {}", windows::core::Error::from_thread()));
        }
        Ok(Self(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// 已收到响应头的请求; 字段按声明顺序释放, 先关请求再关连接和会话.
struct Response {
    request: Handle,
    _connect: Handle,
    _session: Handle,
    status: u32,
}

/// 下载 url 的全部内容. url 只能是 https; 非 200 响应当作失败.
/// 每读到一块就报告一次已收到的字节数和总大小 (服务器没给 Content-Length 时为 None);
/// progress 返回 false 时中止并返回 Err(CANCELED).
pub fn download(
    url: &str,
    timeout_secs: u32,
    mut progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<Vec<u8>, String> {
    unsafe {
        let response = send(url, w!("GET"), "", &[], timeout_secs)?;
        if response.status != 200 {
            return Err(format!("服务器返回 HTTP {}", response.status));
        }
        let mut length = 0u32;
        let mut size = size_of::<u32>() as u32;
        let total = WinHttpQueryHeaders(
            response.request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut length as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()
        .map(|_| length as u64);
        read_body(&response, |received| progress(received, total))
    }
}

/// POST 一段 JSON, 返回响应正文. bearer 是 Authorization 头里的令牌; 非 200 响应当作失败, 说明里带上服务器返回的内容.
pub fn post_json(url: &str, bearer: &str, body: &[u8], timeout_secs: u32) -> Result<Vec<u8>, String> {
    let headers = format!("Content-Type: application/json\r\nAuthorization: Bearer {bearer}");
    unsafe {
        let response = send(url, w!("POST"), &headers, body, timeout_secs)?;
        let text = read_body(&response, |_| true)?;
        if response.status != 200 {
            return Err(format!("服务器返回 HTTP {}: {}", response.status, String::from_utf8_lossy(&text)));
        }
        Ok(text)
    }
}

/// 发出请求并收下响应头. url 只能是 https.
unsafe fn send(url: &str, verb: PCWSTR, headers: &str, body: &[u8], timeout_secs: u32) -> Result<Response, String> {
    let rest = url.strip_prefix("https://").expect("https url");
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let timeout = (timeout_secs * 1000) as i32;

    unsafe {
        let session = Handle::new(
            WinHttpOpen(w!("QRun"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "初始化网络",
        )?;
        WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout).map_err(|e| e.to_string())?;
        let host = wide(host);
        let connect = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0), "连接服务器")?;
        let path = wide(path);
        let request = Handle::new(
            WinHttpOpenRequest(
                connect.0,
                verb,
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "创建请求",
        )?;
        // 头不带结尾的 0, 长度由切片给出
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(&headers[..]),
            (!body.is_empty()).then_some(body.as_ptr() as *const c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|e| format!("发送请求失败: {e}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("接收响应失败: {e}"))?;

        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| e.to_string())?;
        Ok(Response { request, _connect: connect, _session: session, status })
    }
}

/// 读完响应正文; 每读一块前报告已收到的字节数, progress 返回 false 时中止并返回 Err(CANCELED).
unsafe fn read_body(response: &Response, mut progress: impl FnMut(u64) -> bool) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        if !progress(body.len() as u64) {
            return Err(CANCELED.into());
        }
        let mut available = 0u32;
        unsafe { WinHttpQueryDataAvailable(response.request.0, &mut available) }
            .map_err(|e| format!("读取数据失败: {e}"))?;
        if available == 0 {
            return Ok(body);
        }
        let start = body.len();
        body.resize(start + available as usize, 0);
        let mut read = 0u32;
        unsafe { WinHttpReadData(response.request.0, body[start..].as_mut_ptr() as *mut c_void, available, &mut read) }
            .map_err(|e| format!("读取数据失败: {e}"))?;
        body.truncate(start + read as usize);
    }
}
