use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// Member operations that expose a container's index-insensitive element cell.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContainerOperation {
    Access {
        method: String,
        arity: usize,
    },
    Store {
        method: String,
        arity: usize,
        value: usize,
        #[serde(default)]
        entry: Option<usize>,
    },
    Iterator {
        method: String,
        arity: usize,
        #[serde(default)]
        member: Option<String>,
    },
}

impl ContainerOperation {
    pub fn method(&self) -> &str {
        match self {
            Self::Access { method, .. }
            | Self::Store { method, .. }
            | Self::Iterator { method, .. } => method,
        }
    }

    pub fn arity(&self) -> usize {
        match self {
            Self::Access { arity, .. }
            | Self::Store { arity, .. }
            | Self::Iterator { arity, .. } => *arity,
        }
    }
}

/// C++ template argument and member declarations used by a container model.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerModel {
    pub name: String,
    pub declaration_aliases: Option<Vec<String>>,
    pub element: usize,
    #[serde(default)]
    pub initializer_list: bool,
    #[serde(default)]
    pub initializer_entry: Option<usize>,
    /// Template parameter used by the list constructor's optional allocator.
    #[serde(default)]
    pub initializer_allocator: Option<usize>,
    #[serde(default)]
    pub operations: Vec<ContainerOperation>,
}

impl ContainerModel {
    pub fn declaration_aliases(&self) -> &[String] {
        self.declaration_aliases.as_deref().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContainerModels(BTreeMap<String, ContainerModel>);

fn normalize_model_name(name: &str) -> &str {
    name.trim().trim_start_matches("::")
}

impl ContainerModels {
    pub fn builtin() -> &'static Arc<Self> {
        #[derive(Deserialize)]
        struct Config {
            container: Vec<ContainerModel>,
        }

        static BUILTIN: OnceLock<Arc<ContainerModels>> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            let config: Config = toml::from_str(include_str!("container_models.toml"))
                .expect("invalid built-in container models");
            let mut models = Self::default();
            for model in config.container {
                models
                    .register(model)
                    .expect("invalid built-in container model");
            }
            Arc::new(models)
        })
    }

    pub fn register(&mut self, mut model: ContainerModel) -> Result<(), String> {
        let name = normalize_model_name(&model.name);
        if name.is_empty() || name.contains(['<', '>']) || name.contains(char::is_whitespace) {
            return Err(format!(
                "container {:?}: expected a class template name",
                model.name
            ));
        }
        for alias in model.declaration_aliases.iter_mut().flatten() {
            let spelling = normalize_model_name(alias);
            if spelling.is_empty()
                || spelling.contains(['<', '>'])
                || spelling.contains(char::is_whitespace)
            {
                return Err(format!(
                    "container {name:?}: invalid declaration alias {alias:?}"
                ));
            }
            *alias = spelling.to_owned();
        }
        for op in &model.operations {
            let method = op.method();
            if method.is_empty() || method.contains("::") || method.contains(char::is_whitespace) {
                return Err(format!(
                    "container {name:?}: expected an unqualified method name, got {method:?}"
                ));
            }
            match op {
                ContainerOperation::Store { arity, value, .. } if value >= arity => {
                    return Err(format!("container {name:?}: {method:?} value index {value} exceeds the argument list of length {arity}"));
                }
                ContainerOperation::Iterator {
                    member: Some(member),
                    ..
                } if member.is_empty()
                    || member.contains("::")
                    || member.contains(char::is_whitespace) =>
                {
                    return Err(format!("container {name:?}: expected an unqualified iterator field, got {member:?}"));
                }
                _ => {}
            }
        }
        if model.initializer_entry.is_some() && !model.initializer_list {
            return Err(format!(
                "container {name:?}: initializer_entry requires initializer_list"
            ));
        }
        if model.initializer_allocator.is_some() && !model.initializer_list {
            return Err(format!(
                "container {name:?}: initializer_allocator requires initializer_list"
            ));
        }
        if !model.initializer_list && model.operations.is_empty() {
            self.0.remove(name);
            return Ok(());
        }
        if model.declaration_aliases.is_none() {
            model.declaration_aliases = self
                .0
                .get(name)
                .and_then(|previous| previous.declaration_aliases.clone());
        }
        model.name = name.to_owned();
        self.0.insert(model.name.clone(), model);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ContainerModel> {
        let name = normalize_model_name(name);
        self.0.get(name).or_else(|| {
            self.0.values().find(|model| {
                model
                    .declaration_aliases()
                    .iter()
                    .any(|alias| alias == name)
            })
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &ContainerModel> {
        self.0.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_accepts_global_scope_for_names_and_aliases() {
        let models = ContainerModels::builtin();
        for spelling in ["::std::vector", "  ::std::__1::vector  "] {
            assert_eq!(models.get(spelling).unwrap().name, "std::vector");
        }
    }
}
