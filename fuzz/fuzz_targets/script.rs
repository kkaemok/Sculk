#![no_main]
use libfuzzer_sys::fuzz_target;
use sculk_script::{Compiler, Limits, Source};
fuzz_target!(|data: &[u8]| {
    if data.len() > 8192 {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let compiler = Compiler::new(Limits {
        file_bytes: 8192,
        depth: 8,
        instructions: 512,
        string_bytes: 4096,
        diagnostics: 8,
        ..Limits::default()
    });
    let result = compiler.compile(&[Source {
        filename: "fuzz.sculk",
        text,
    }]);
    match result {
        Ok(program) => {
            assert!(program.instruction_count() <= 512);
            let _ = program.install(sculk_bot::Bot::new(sculk_bot::Config::default()).unwrap());
        }
        Err(errors) => {
            assert!(!errors.is_empty());
            assert!(errors.len() <= 8);
        }
    }
});
