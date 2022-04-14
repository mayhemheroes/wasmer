//! The import module contains the implementation data structures and helper functions used to
//! manipulate and access a wasm module's imports including memories, tables, globals, and
//! functions.
use crate::{Exportable, Exports, Extern, Module};
use std::collections::HashMap;
use std::fmt;
use wasmer_engine::{Export, ImportError, LinkError};

/// TODO add doc
#[derive(Clone, Default)]
pub struct Imports {
    map: HashMap<(String, String), Extern>,
}

impl Imports {
    /// Create a new `Imports`.
    pub fn new() -> Self {
        Default::default()
    }

    /// Gets an export given a module and a name
    ///
    /// # Usage
    /// ```ignore
    /// let mut import_object = Imports::new();
    /// import_object.get_export("module", "name");
    /// ```
    pub fn get_export(&self, module: &str, name: &str) -> Option<Export> {
        if self
            .map
            .contains_key(&(module.to_string(), name.to_string()))
        {
            let ext = &self.map[&(module.to_string(), name.to_string())];
            return Some(ext.to_export());
        }
        None
    }

    /// Returns true if the Imports contains namespace with the provided name.
    pub fn contains_namespace(&self, name: &str) -> bool {
        self.map.keys().any(|(k, _)| (k == name))
    }

    /// Register a list of externs into a namespace.
    ///
    /// # Usage:
    /// ```ignore
    /// # use wasmer::{Imports, Exports};
    /// let mut exports = Exports::new()
    /// exports.insert("memory", memory);
    ///
    /// let mut import_object = Imports::new();
    /// import_object.register("env", exports);
    /// // ...
    /// ```
    pub fn register_namespace(
        &mut self,
        ns: &str,
        contents: impl IntoIterator<Item = (String, Extern)>,
    ) {
        for (name, extern_) in contents.into_iter() {
            self.map.insert((ns.to_string(), name.clone()), extern_);
        }
    }

    /// TODO: Add doc
    pub fn define(&mut self, ns: &str, name: &str, val: impl Into<Extern>) {
        self.map
            .insert((ns.to_string(), name.to_string()), val.into());
    }

    /// Returns the contents of a namespace as an `Exports`.
    ///
    /// Returns `None` if the namespace doesn't exist.
    pub fn get_namespace_exports(&self, name: &str) -> Option<Exports> {
        let ret: Exports = self
            .map
            .iter()
            .filter(|((ns, _), _)| ns == name)
            .map(|((_, name), e)| (name.clone(), e.clone()))
            .collect();
        if ret.is_empty() {
            None
        } else {
            Some(ret)
        }
    }

    /// TODO: Add doc
    pub fn imports_for_module(&self, module: &Module) -> Result<Vec<Export>, LinkError> {
        let mut ret = vec![];
        for import in module.imports() {
            if let Some(imp) = self
                .map
                .get(&(import.module().to_string(), import.name().to_string()))
            {
                ret.push(imp.to_export());
            } else {
                return Err(LinkError::Import(
                    import.module().to_string(),
                    import.name().to_string(),
                    ImportError::UnknownImport(import.ty().clone()),
                ));
            }
        }
        Ok(ret)
    }
}

impl IntoIterator for &Imports {
    type IntoIter = std::collections::hash_map::IntoIter<(String, String), Extern>;
    type Item = ((String, String), Extern);

    fn into_iter(self) -> Self::IntoIter {
        self.map.clone().into_iter()
    }
}

impl fmt::Debug for Imports {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        enum SecretMap {
            Empty,
            Some(usize),
        }

        impl SecretMap {
            fn new(len: usize) -> Self {
                if len == 0 {
                    Self::Empty
                } else {
                    Self::Some(len)
                }
            }
        }

        impl fmt::Debug for SecretMap {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                match self {
                    Self::Empty => write!(f, "(empty)"),
                    Self::Some(len) => write!(f, "(... {} item(s) ...)", len),
                }
            }
        }

        f.debug_struct("Imports")
            .field("map", &SecretMap::new(self.map.len()))
            .finish()
    }
}

// The import! macro for Imports

/// Generate an [`Imports`] easily with the `imports!` macro.
///
/// [`Imports`]: struct.Imports.html
///
/// # Usage
///
/// ```
/// # use wasmer::{Function, Store};
/// # let store = Store::default();
/// use wasmer::imports;
///
/// let import_object = imports! {
///     "env" => {
///         "foo" => Function::new_native(&store, foo)
///     },
/// };
///
/// fn foo(n: i32) -> i32 {
///     n
/// }
/// ```
#[macro_export]
macro_rules! imports {
    ( $( $ns_name:expr => $ns:tt ),* $(,)? ) => {
        {
            let mut import_object = $crate::Imports::new();

            $({
                let namespace = $crate::import_namespace!($ns);

                import_object.register_namespace($ns_name, namespace);
            })*

            import_object
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! namespace {
    ($( $import_name:expr => $import_item:expr ),* $(,)? ) => {
        $crate::import_namespace!( { $( $import_name => $import_item, )* } )
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! import_namespace {
    ( { $( $import_name:expr => $import_item:expr ),* $(,)? } ) => {{
        let mut namespace = $crate::Exports::new();

        $(
            namespace.insert($import_name, $import_item);
        )*

        namespace
    }};

    ( $namespace:ident ) => {
        $namespace
    };
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::sys::{Global, Store, Val};
    use wasmer_types::Type;

    #[test]
    fn namespace() {
        let store = Store::default();
        let g1 = Global::new(&store, Val::I32(0));
        let namespace = namespace! {
            "happy" => g1
        };
        let imports1 = imports! {
            "dog" => namespace
        };

        let happy_dog_entry = imports1.get_export("dog", "happy").unwrap();

        assert!(if let Export::Global(happy_dog_global) = happy_dog_entry {
            happy_dog_global.from.ty().ty == Type::I32
        } else {
            false
        });
    }

    #[test]
    fn imports_macro_allows_trailing_comma_and_none() {
        use crate::sys::Function;

        let store = Default::default();

        fn func(arg: i32) -> i32 {
            arg + 1
        }

        let _ = imports! {
            "env" => {
                "func" => Function::new_native(&store, func),
            },
        };
        let _ = imports! {
            "env" => {
                "func" => Function::new_native(&store, func),
            }
        };
        let _ = imports! {
            "env" => {
                "func" => Function::new_native(&store, func),
            },
            "abc" => {
                "def" => Function::new_native(&store, func),
            }
        };
        let _ = imports! {
            "env" => {
                "func" => Function::new_native(&store, func)
            },
        };
        let _ = imports! {
            "env" => {
                "func" => Function::new_native(&store, func)
            }
        };
        let _ = imports! {
            "env" => {
                "func1" => Function::new_native(&store, func),
                "func2" => Function::new_native(&store, func)
            }
        };
        let _ = imports! {
            "env" => {
                "func1" => Function::new_native(&store, func),
                "func2" => Function::new_native(&store, func),
            }
        };
    }
}
