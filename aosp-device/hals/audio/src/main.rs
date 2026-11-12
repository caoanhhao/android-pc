//! android.hardware.audio.core HAL for PCs: the primary module (`IModule/default`) over
//! the built-in codec's ALSA PCM devices (`/dev/snd/pcmC*D*{p,c}`).
//!
//! The other audio HAL instances (`IConfig/default`, the `r_submix` and `bluetooth`
//! modules, the effects factory) stay with AOSP's default HAL in the
//! `com.android.hardware.audio` APEX; see `hal.mk`.

mod aidl;
mod cards;
mod config;
mod ctl;
mod module;
mod pcm;
mod stream;
mod worker;

use std::process::ExitCode;

use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::aidl::{BnModule, IModule};
use crate::config::Hardware;
use crate::module::Module;

const LOG_TAG: &str = "android.hardware.audio.service.pc";

/// Binder threads: the framework calls the module and every stream from several threads
/// (AudioFlinger, AudioPolicy, the patch panel) at once.
const BINDER_THREADS: u32 = 8;

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let module = Module::new(Hardware::discover());
    let binder = BnModule::new_binder(module, BinderFeatures::default());
    let service = Service::new(Module::get_descriptor(), "default").extra_threads(BINDER_THREADS);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
