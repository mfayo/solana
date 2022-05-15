use {crate::config::sealevel_config_opt::*, solana_rbpf::vm::Config, std::os::raw::c_int};

/// Sealevel virtual machine config.
#[derive(Default)]
pub struct sealevel_config {
    pub(crate) config: Config,
    pub(crate) no_verify: bool,
}

#[repr(C)]
pub enum sealevel_config_opt {
    SEALEVEL_OPT_NONE,
    SEALEVEL_OPT_NO_VERIFY,
    SEALEVEL_OPT_MAX_CALL_DEPTH,
    SEALEVEL_STACK_FRAME_SIZE,
    SEALEVEL_ENABLE_STACK_FRAME_GAPS,
    SEALEVEL_INSTRUCTION_METER_CHECKPOINT_DISTANCE,
    SEALEVEL_ENABLE_INSTRUCTION_METER,
    SEALEVEL_ENABLE_INSTRUCTION_TRACING,
    SEALEVEL_ENABLE_SYMBOL_AND_SECTION_LABELS,
    SEALEVEL_DISABLE_UNRESOLVED_SYMBOLS_AT_RUNTIME,
    SEALEVEL_REJECT_BROKEN_ELFS,
    SEALEVEL_NOOP_INSTRUCTION_RATIO,
    SEALEVEL_SANITIZE_USER_PROVIDED_VALUES,
    SEALEVEL_ENCRYPT_ENVIRONMENT_REGISTERS,
    SEALEVEL_DISABLE_DEPRECATED_LOAD_INSTRUCTIONS,
    SEALEVEL_SYSCALL_BPF_FUNCTION_HASH_COLLISION,
    SEALEVEL_REJECT_CALLX_R10,
    SEALEVEL_DYNAMIC_STACK_FRAMES,
    SEALEVEL_ENABLE_SDIV,
    SEALEVEL_OPTIMIZE_RODATA,
    SEALEVEL_STATIC_SYSCALLS,
    SEALEVEL_ENABLE_ELF_VADDR,
}

impl sealevel_config {
    fn new() -> Self {
        Self::default()
    }
}

/// Creates a new Sealevel machine config.
///
/// # Safety
/// Call `sealevel_config_free` on the return value after you are done using it.
/// Failure to do so results in a memory leak.
#[no_mangle]
pub extern "C" fn sealevel_config_new() -> *mut sealevel_config {
    let wrapper = sealevel_config::new();
    Box::into_raw(Box::new(wrapper))
}

macro_rules! va_bool {
    ($args:ident) => {
        $args.arg::<c_int>() != 0
    };
}

/// Sets a config option given the config key and exactly one value arg.
///
/// # Safety
/// Avoid the following undefined behavior:
/// - Passing the wrong argument type as the config value (each key documents the expected value).
/// - Passing any other amount of function arguments than exactly 3.
///
/// cbindgen:ignore
/// https://github.com/eqrion/cbindgen/issues/376
#[no_mangle]
pub unsafe extern "C" fn sealevel_config_setopt(
    config: *mut sealevel_config,
    key: sealevel_config_opt,
    mut args: ...
) {
    match key {
        SEALEVEL_OPT_NONE => (),
        SEALEVEL_OPT_NO_VERIFY => (*config).no_verify = va_bool!(args),
        SEALEVEL_OPT_MAX_CALL_DEPTH => (*config).config.max_call_depth = args.arg::<usize>(),
        SEALEVEL_STACK_FRAME_SIZE => (*config).config.stack_frame_size = args.arg::<usize>(),
        SEALEVEL_ENABLE_STACK_FRAME_GAPS => {
            (*config).config.enable_stack_frame_gaps = va_bool!(args)
        }
        SEALEVEL_INSTRUCTION_METER_CHECKPOINT_DISTANCE => {
            (*config).config.instruction_meter_checkpoint_distance = args.arg::<usize>()
        }
        SEALEVEL_ENABLE_INSTRUCTION_METER => {
            (*config).config.enable_instruction_meter = va_bool!(args)
        }
        SEALEVEL_ENABLE_INSTRUCTION_TRACING => {
            (*config).config.enable_instruction_tracing = va_bool!(args)
        }
        SEALEVEL_ENABLE_SYMBOL_AND_SECTION_LABELS => {
            (*config).config.enable_symbol_and_section_labels = va_bool!(args)
        }
        SEALEVEL_DISABLE_UNRESOLVED_SYMBOLS_AT_RUNTIME => {
            (*config).config.disable_unresolved_symbols_at_runtime = va_bool!(args)
        }
        SEALEVEL_REJECT_BROKEN_ELFS => (*config).config.reject_broken_elfs = va_bool!(args),
        SEALEVEL_NOOP_INSTRUCTION_RATIO => {
            (*config).config.noop_instruction_ratio = args.arg::<f64>()
        }
        SEALEVEL_SANITIZE_USER_PROVIDED_VALUES => {
            (*config).config.sanitize_user_provided_values = va_bool!(args)
        }
        SEALEVEL_ENCRYPT_ENVIRONMENT_REGISTERS => {
            (*config).config.encrypt_environment_registers = va_bool!(args)
        }
        SEALEVEL_DISABLE_DEPRECATED_LOAD_INSTRUCTIONS => {
            (*config).config.disable_deprecated_load_instructions = va_bool!(args)
        }
        SEALEVEL_SYSCALL_BPF_FUNCTION_HASH_COLLISION => {
            (*config).config.syscall_bpf_function_hash_collision = va_bool!(args)
        }
        SEALEVEL_REJECT_CALLX_R10 => (*config).config.reject_callx_r10 = va_bool!(args),
        SEALEVEL_DYNAMIC_STACK_FRAMES => (*config).config.dynamic_stack_frames = va_bool!(args),
        SEALEVEL_ENABLE_SDIV => (*config).config.enable_sdiv = va_bool!(args),
        SEALEVEL_OPTIMIZE_RODATA => (*config).config.optimize_rodata = va_bool!(args),
        SEALEVEL_STATIC_SYSCALLS => (*config).config.static_syscalls = va_bool!(args),
        SEALEVEL_ENABLE_ELF_VADDR => (*config).config.enable_elf_vaddr = va_bool!(args),
    }
}

/// Releases resources associated with a Sealevel machine config.
///
/// # Safety
/// Avoid the following undefined behavior:
/// - Calling this function given a string that's _not_ the return value of `sealevel_config_new`.
/// - Calling this function more than once on the same object (double free).
/// - Using the config object after calling this function (use-after-free).
#[no_mangle]
pub unsafe extern "C" fn sealevel_config_free(config: *mut sealevel_config) {
    drop(Box::from_raw(config))
}
