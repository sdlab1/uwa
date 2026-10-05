//! Built-in stealth scripts. Pure JS strings; no browser dependency.

use crate::{StealthPack, StealthScript};

fn s(name: &str, js: &str) -> StealthScript {
    StealthScript {
        name: name.into(),
        js: js.into(),
        apply_before_load: true,
    }
}

const WEBDRIVER: &str = r#"Object.defineProperty(navigator,'webdriver',{get:()=>undefined});"#;

const CHROME_RUNTIME: &str = r#"if(!window.chrome){window.chrome={};}
if(!window.chrome.runtime){window.chrome.runtime={};}"#;

const LANGUAGES: &str = r#"Object.defineProperty(navigator,'languages',{get:()=>['en-US','en']});"#;

const PERMISSIONS: &str = r#"const _p=navigator.permissions;
if(_p && !_p.query){navigator.permissions={query:(d)=>Promise.resolve({state:'prompt'})};}"#;

/// Scripts that every provider needs: webdriver flag, chrome runtime, languages.
pub fn default_pack() -> StealthPack {
    StealthPack::new()
        .with_script(s("webdriver", WEBDRIVER))
        .with_script(s("chrome_runtime", CHROME_RUNTIME))
        .with_script(s("languages", LANGUAGES))
}

/// Everything in [`default_pack`] plus permission-prompt normalization.
pub fn full_pack() -> StealthPack {
    default_pack().with_script(s("permissions", PERMISSIONS))
}
