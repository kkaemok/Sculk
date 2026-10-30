#![no_main]
use libfuzzer_sys::fuzz_target;
use sculk_script::{
    addon::{Registry, Syntax},
    Compiler, Limits, Source, Type, Value,
};
fuzz_target!(|data: &[u8]| {
    if data.len() > 8192 {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (pattern, body) = text.split_once('\n').unwrap_or((text, ""));
    let mut registry = Registry::new();
    let syntax = Syntax::new(pattern)
        .parameter("value", Type::Integer)
        .returns(Type::Integer)
        .cost(1);
    if registry
        .expression(syntax, |_, args| {
            Ok(args.first().cloned().unwrap_or(Value::Integer(0)))
        })
        .is_ok()
    {
        assert_eq!(registry.len(), 1);
        let result = Compiler::new(Limits {
            file_bytes: 8192,
            depth: 8,
            instructions: 512,
            diagnostics: 8,
            ..Limits::default()
        })
        .with_registry(registry)
        .compile(&[Source {
            filename: "fuzz.sc",
            text: body,
        }]);
        if let Ok(program) = result {
            assert!(program.instruction_count() <= 512);
        }
    } else {
        assert!(registry.is_empty());
    }
});
