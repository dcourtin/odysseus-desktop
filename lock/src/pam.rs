//! Vérification du mot de passe par PAM (service « odysseus-lock »), sans
//! dépendance : quelques déclarations de libpam suffisent.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_CONV_ERR: c_int = 19;
const PAM_BUF_ERR: c_int = 5;
const PAM_REFRESH_CRED: c_int = 0x0010;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut c_void, errnum: c_int) -> *const c_char;
}

unsafe extern "C" {
    fn calloc(n: usize, size: usize) -> *mut c_void;
    fn strdup(s: *const c_char) -> *mut c_char;
}

/// Répond au mot de passe pour chaque question masquée de PAM.
extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    password: *mut c_void,
) -> c_int {
    if count <= 0 || messages.is_null() || responses.is_null() || password.is_null() {
        return PAM_CONV_ERR;
    }
    // PAM libère lui-même ce tableau et les chaînes : allocation par la libc
    let replies =
        unsafe { calloc(count as usize, std::mem::size_of::<PamResponse>()) } as *mut PamResponse;
    if replies.is_null() {
        return PAM_BUF_ERR;
    }
    let password = password as *const c_char;
    for i in 0..count as usize {
        let message = unsafe { &**messages.add(i) };
        if message.msg_style == PAM_PROMPT_ECHO_OFF || message.msg_style == PAM_PROMPT_ECHO_ON {
            unsafe { (*replies.add(i)).resp = strdup(password) };
        }
    }
    unsafe { *responses = replies };
    PAM_SUCCESS
}

/// Vérifie le mot de passe de `user` ; en cas d'échec, le message de PAM.
pub fn authenticate(user: &str, password: &str) -> Result<(), String> {
    let service = CString::new("odysseus-lock").unwrap();
    let user = CString::new(user).map_err(|_| "nom d'utilisateur invalide".to_string())?;
    let password = CString::new(password).map_err(|_| "mot de passe invalide".to_string())?;
    let conv = PamConv {
        conv: conversation,
        appdata_ptr: password.as_ptr() as *mut c_void,
    };
    let mut handle: *mut c_void = std::ptr::null_mut();
    let rc = unsafe { pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle) };
    if rc != PAM_SUCCESS {
        return Err(format!("PAM indisponible ({rc})"));
    }
    let mut rc = unsafe { pam_authenticate(handle, 0) };
    if rc == PAM_SUCCESS {
        rc = unsafe { pam_acct_mgmt(handle, 0) };
    }
    if rc == PAM_SUCCESS {
        // Renouvelle les jetons éventuels (Kerberos…) ; sans effet sinon
        unsafe { pam_setcred(handle, PAM_REFRESH_CRED) };
    }
    let result = if rc == PAM_SUCCESS {
        Ok(())
    } else {
        let text = unsafe { pam_strerror(handle, rc) };
        Err(if text.is_null() {
            format!("échec ({rc})")
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        })
    };
    unsafe { pam_end(handle, rc) };
    result
}
