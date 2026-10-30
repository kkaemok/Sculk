#![no_main]
use libfuzzer_sys::fuzz_target;
use sculk_script::{
    addon::{Capability, Registry, Syntax},
    Compiler, Limits, Source, Type,
};
fuzz_target!(|data: &[u8]| {
    if data.len() > 8192 {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut registry = Registry::new();
    registry
        .expression(
            Syntax::new("addon score %value%")
                .parameter("value", Type::Integer)
                .returns(Type::Integer)
                .cost(1),
            |_, args| Ok(args[0].clone()),
        )
        .unwrap();
    registry
        .condition(
            Syntax::new("addon valid %value%")
                .parameter("value", Type::Integer)
                .cost(1),
            |_, args| Ok(args[0].as_integer().is_some()),
        )
        .unwrap();
    registry
        .effect(
            Syntax::new("addon emit %value%")
                .parameter("value", Type::String)
                .cost(1)
                .requires(Capability::Reply),
            |ctx, args| async move {
                ctx.reply(args[0].as_string().unwrap()).await?;
                Ok(())
            },
        )
        .unwrap();
    registry
        .event(
            Syntax::new("addon arrival").cost(1),
            sculk_bot::EventKind::Message,
            |_| Ok(true),
        )
        .unwrap();
    let compiler = Compiler::new(Limits {
        file_bytes: 8192,
        depth: 8,
        instructions: 512,
        string_bytes: 4096,
        diagnostics: 8,
        ..Limits::default()
    })
    .with_registry(registry)
    .capabilities([Capability::Reply]);
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
