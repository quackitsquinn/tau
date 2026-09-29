use std::io::{Read, Write};

use rustix::fs::FlockOperation;
use syn::{Expr, LitStr};

pub fn derive_should_trace_target(expr: LitStr) -> proc_macro::TokenStream {
    let log_filters = option_env!("TAU_TRACE_FEATURES")
        .map(|features| {
            features
                .split(',')
                .map(str::trim)
                .map(str::to_lowercase)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if log_filters.contains(&"all".to_string()) {
        return quote::quote! { true }.into();
    }

    let _ = add_trace_target(&expr);

    if log_filters.contains(&expr.value().to_lowercase()) {
        quote::quote! { true }.into()
    } else {
        quote::quote! { false }.into()
    }
}

fn add_trace_target(expr: &LitStr) -> std::io::Result<()> {
    let target = expr.value();

    let mut list = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open("target/trace_targets.txt")?;

    rustix::fs::flock(&list, FlockOperation::LockExclusive)?;

    let mut contents = String::new();
    list.read_to_string(&mut contents)?;
    if contents.contains(&target) {
        rustix::fs::flock(&list, FlockOperation::Unlock)?;
        return Ok(());
    }

    writeln!(list, "{}", target)?;
    list.flush()?;
    rustix::fs::flock(&list, FlockOperation::Unlock)?;

    Ok(())
}
