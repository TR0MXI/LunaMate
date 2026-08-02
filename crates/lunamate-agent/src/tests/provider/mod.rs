use crate::{config::AppLanguage, tools::OutfitOption};

const LANGUAGE: AppLanguage = AppLanguage::English;

fn outfit(id: &str, label: &str) -> OutfitOption {
    OutfitOption::new(id, label)
}

mod request;
mod stream;
mod timeouts;
mod tools;
