//! Package relocation for the fat jar: `[package.relocate]` (SPEC §9.9).
//!
//! A relocation renames a package, and every package under it, throughout the
//! fat jar: the classes move to the new name, and every reference to them —
//! from the project's own classes and from every dependency's — is rewritten to
//! match. This is Maven Shade's relocation. It lets a jar that will share a
//! classpath with someone else's copy of a library carry its own.
//!
//! Rewriting a class means rewriting its constant pool, and nothing else. Every
//! name a class file holds — its own, its superclass's, a field's type, a
//! method descriptor, a generic signature, an annotation's type, a string
//! constant — is a `CONSTANT_Utf8` entry, and every other structure refers to
//! those entries by index. Only the entries' contents change, never their
//! number or order, so every index stays valid and the bytes after the pool are
//! copied through untouched. No bytecode is parsed and no crate is needed.
//!
//! Within an entry a name is recognised where one can start: at the start of
//! the text (an internal name `com/google/common/base/Strings`, a class name
//! `com.google.common.base.Strings` in a string constant, a resource path
//! with or without its leading `/`), or after the `L` that opens a class type
//! in a descriptor or signature (`(ILcom/google/common/base/Strings;)V`). A
//! name matches a relocation when it lies in the package or under it:
//! `com.google.common` covers `com.google.common.base.Strings` but neither
//! `com.google.commonx.Foo` nor a class named `com.google.common`. The same
//! rule, with every non-name character as a boundary, rewrites the class
//! names in `META-INF/services/` files and Spring's and Groovy's registries.
//!
//! What is not rewritten: a class name in the middle of a string constant
//! (`"cannot load com.google.common.Foo"`), any other resource's contents, and
//! Kotlin's packed `@Metadata` strings. Those are the same limits Shade has.

use crate::manifest::Relocation;

/// A fat jar's relocations, most specific `from` first, so a nested package's
/// own relocation wins over its parent's.
#[derive(Debug, Clone, Default)]
pub struct Relocator {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    /// `com/google/common` and `com.google.common`.
    from: [Vec<u8>; 2],
    /// `shaded/guava` and `shaded.guava`, in the same order.
    to: [Vec<u8>; 2],
    /// Dotted class names and packages the relocation leaves alone.
    exclude: Vec<Exclude>,
}

#[derive(Debug, Clone)]
enum Exclude {
    /// A class, and the classes nested in it.
    Class(Vec<u8>),
    /// A package and everything under it, stored with its trailing `.`.
    Package(Vec<u8>),
}

/// Where in a text a name may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// A `CONSTANT_Utf8` entry: the start of the text, a resource path's
    /// leading `/`, or a descriptor's `L`.
    Constant,
    /// A jar entry name or a class name: the start only.
    Whole,
    /// A registry file: anywhere a name character does not precede it.
    Text,
}

/// The characters a descriptor or signature may put before the `L` that opens
/// a class type, other than a primitive's letter or an array's `[`.
const BEFORE_CLASS_TYPE: &[u8] = b"();<>:^+-*";

/// Primitive type letters and the array marker, which may run up to that `L`:
/// `(IJ[Lcom/google/common/base/Strings;)V`.
const TYPE_PREFIX: &[u8] = b"BCDFIJSZ[";

impl Relocator {
    #[must_use]
    pub fn new(relocations: &[Relocation]) -> Relocator {
        let mut rules: Vec<Rule> = relocations
            .iter()
            .map(|r| Rule {
                from: [slashed(&r.from), r.from.clone().into_bytes()],
                to: [slashed(&r.to), r.to.clone().into_bytes()],
                exclude: r
                    .exclude
                    .iter()
                    .map(|e| match e.strip_suffix(".*") {
                        Some(package) => Exclude::Package(format!("{package}.").into_bytes()),
                        None => Exclude::Class(e.clone().into_bytes()),
                    })
                    .collect(),
            })
            .collect();
        // Longest first; the sort is stable, so equal lengths keep manifest order.
        rules.sort_by_key(|r| std::cmp::Reverse(r.from[0].len()));
        Relocator { rules }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Where a jar entry goes: a class or resource in a relocated package moves
    /// with it, under `META-INF/versions/<n>/` as well as at the root, and a
    /// `META-INF/services/` file follows the interface it is named for.
    #[must_use]
    pub fn entry_name(&self, name: &str) -> String {
        if self.is_empty() {
            return name.to_string();
        }
        if let Some(service) = name.strip_prefix("META-INF/services/") {
            return format!("META-INF/services/{}", self.class_name(service));
        }
        if let Some(rest) = name.strip_prefix("META-INF/versions/")
            && let Some((version, path)) = rest.split_once('/')
            && !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit())
        {
            return format!("META-INF/versions/{version}/{}", self.whole(path));
        }
        self.whole(name)
    }

    /// A fully-qualified class name, relocated: the jar manifest's
    /// `Main-Class`, a service file's name.
    #[must_use]
    pub fn class_name(&self, name: &str) -> String {
        self.whole(name)
    }

    fn whole(&self, text: &str) -> String {
        match self.rewrite(text.as_bytes(), Context::Whole) {
            // Only ASCII is ever replaced, at character boundaries, so the
            // result is as valid UTF-8 as the input was.
            Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            None => text.to_string(),
        }
    }

    /// A registry file — a `META-INF/services/` file, `spring.factories`, a
    /// Groovy extension-module descriptor — with the class names in it
    /// relocated.
    #[must_use]
    pub fn text(&self, bytes: Vec<u8>) -> Vec<u8> {
        if self.is_empty() {
            return bytes;
        }
        self.rewrite(&bytes, Context::Text).unwrap_or(bytes)
    }

    /// A class file with its constant pool relocated, or the same bytes when
    /// nothing in it names a relocated package.
    ///
    /// # Errors
    ///
    /// A description of the problem when `bytes` is not a class file this can
    /// read: a bad magic number, a truncated pool, a constant-pool tag no JVM
    /// defines, or a name that would outgrow the 65535 bytes an entry can hold.
    pub fn class_file(&self, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        if self.is_empty() {
            return Ok(bytes);
        }
        let truncated = || "the constant pool is truncated".to_string();
        if bytes.get(..4) != Some(&[0xCA, 0xFE, 0xBA, 0xBE]) {
            return Err("not a class file (bad magic number)".to_string());
        }
        let count = u16_at(&bytes, 8).ok_or_else(truncated)?;

        let mut out: Option<Vec<u8>> = None;
        let mut copied = 0;
        let mut at = 10;
        let mut index = 1;
        while index < count {
            let tag = *bytes.get(at).ok_or_else(truncated)?;
            let size = match tag {
                1 => {
                    let len = usize::from(u16_at(&bytes, at + 1).ok_or_else(truncated)?);
                    let text = bytes.get(at + 3..at + 3 + len).ok_or_else(truncated)?;
                    if let Some(relocated) = self.rewrite(text, Context::Constant) {
                        let len = u16::try_from(relocated.len()).map_err(|_| {
                            format!(
                                "relocating `{}` would make it longer than a constant can be",
                                String::from_utf8_lossy(text)
                            )
                        })?;
                        let o = out.get_or_insert_with(|| Vec::with_capacity(bytes.len() + 256));
                        o.extend_from_slice(&bytes[copied..at]);
                        o.push(1);
                        o.extend_from_slice(&len.to_be_bytes());
                        o.extend_from_slice(&relocated);
                        copied = at + 3 + text.len();
                    }
                    3 + text.len()
                }
                // Class, String, MethodType, Module, Package.
                7 | 8 | 16 | 19 | 20 => 3,
                // MethodHandle.
                15 => 4,
                // Integer, Float, the member refs, NameAndType, Dynamic,
                // InvokeDynamic.
                3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => 5,
                // Long and Double take two slots.
                5 | 6 => {
                    index += 1;
                    9
                }
                other => {
                    return Err(format!(
                        "constant-pool tag {other} is not one a JVM defines"
                    ));
                }
            };
            at += size;
            index += 1;
        }
        if at > bytes.len() {
            return Err(truncated());
        }
        Ok(match out {
            Some(mut o) => {
                o.extend_from_slice(&bytes[copied..]);
                o
            }
            None => bytes,
        })
    }

    /// `text` with every relocated name in it rewritten, or `None` when it
    /// holds none. The text is scanned once, left to right, and a replacement
    /// is never scanned again, so a package relocated into itself
    /// (`com.foo` → `com.foo.shaded`) terminates.
    fn rewrite(&self, text: &[u8], context: Context) -> Option<Vec<u8>> {
        let mut out: Option<Vec<u8>> = None;
        let mut copied = 0;
        let mut i = 0;
        while i < text.len() {
            if can_start(text, i, context)
                && let Some((consumed, replacement)) = self.match_at(text, i)
            {
                let o = out.get_or_insert_with(|| Vec::with_capacity(text.len() + 32));
                o.extend_from_slice(&text[copied..i]);
                o.extend_from_slice(replacement);
                i += consumed;
                copied = i;
                continue;
            }
            i += 1;
        }
        out.map(|mut o| {
            o.extend_from_slice(&text[copied..]);
            o
        })
    }

    /// The first rule whose package the name at `text[i..]` lies in: how many
    /// bytes of `from` it spans, and what replaces them.
    fn match_at(&self, text: &[u8], i: usize) -> Option<(usize, &[u8])> {
        let rest = &text[i..];
        for rule in &self.rules {
            for (form, separator) in [(0, b'/'), (1, b'.')] {
                let from = &rule.from[form];
                if !rest.starts_with(from) {
                    continue;
                }
                match rest.get(from.len()) {
                    Some(&next) if next == separator => {}
                    // The package's own name, standing alone: dotted, as
                    // `Package.getPackage` takes it. Slashed, it would be a class.
                    None if i == 0 && separator == b'.' => {}
                    _ => continue,
                }
                if rule.excludes(&name_at(rest, separator)) {
                    continue;
                }
                return Some((from.len(), &rule.to[form]));
            }
        }
        None
    }
}

impl Rule {
    fn excludes(&self, dotted: &[u8]) -> bool {
        self.exclude.iter().any(|e| match e {
            Exclude::Class(class) => {
                dotted == class.as_slice()
                    || (dotted.starts_with(class) && dotted.get(class.len()) == Some(&b'$'))
            }
            Exclude::Package(prefix) => dotted.starts_with(prefix),
        })
    }
}

fn slashed(dotted: &str) -> Vec<u8> {
    dotted.replace('.', "/").into_bytes()
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// A byte that can be part of a Java name. Anything at or above `0x80` is part
/// of a multi-byte character, which in a name is a letter.
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// The name that starts `text`, in its `separator` form, as a dotted name —
/// what an exclusion is checked against.
fn name_at(text: &[u8], separator: u8) -> Vec<u8> {
    text.iter()
        .take_while(|&&b| is_name_byte(b) || b == separator)
        .map(|&b| if b == b'/' { b'.' } else { b })
        .collect()
}

fn can_start(text: &[u8], i: usize, context: Context) -> bool {
    if i == 0 {
        return true;
    }
    match context {
        Context::Whole => false,
        Context::Text => {
            let before = text[i - 1];
            !is_name_byte(before) && before != b'.' && before != b'/'
        }
        Context::Constant => {
            // `/com/google/common/messages.properties`, for `getResource`.
            if i == 1 && text[0] == b'/' {
                return true;
            }
            if text[i - 1] != b'L' {
                return false;
            }
            // Back from the `L`, over primitive letters and `[`, to what opens
            // the type: a descriptor's punctuation, or the start.
            let mut j = i - 1;
            while j > 0 {
                let b = text[j - 1];
                if TYPE_PREFIX.contains(&b) {
                    j -= 1;
                } else {
                    return BEFORE_CLASS_TYPE.contains(&b);
                }
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relocator(rules: &[(&str, &str, &[&str])]) -> Relocator {
        Relocator::new(
            &rules
                .iter()
                .map(|(from, to, exclude)| Relocation {
                    from: (*from).to_string(),
                    to: (*to).to_string(),
                    exclude: exclude.iter().map(|e| (*e).to_string()).collect(),
                })
                .collect::<Vec<_>>(),
        )
    }

    fn guava() -> Relocator {
        relocator(&[("com.google.common", "shaded.guava", &[])])
    }

    fn constant(r: &Relocator, text: &str) -> String {
        r.rewrite(text.as_bytes(), Context::Constant)
            .map_or_else(|| text.to_string(), |b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn internal_names_descriptors_and_signatures_are_relocated() {
        let r = guava();
        for (before, after) in [
            (
                "com/google/common/base/Strings",
                "shaded/guava/base/Strings",
            ),
            (
                "Lcom/google/common/base/Strings;",
                "Lshaded/guava/base/Strings;",
            ),
            (
                "[[Lcom/google/common/base/Strings;",
                "[[Lshaded/guava/base/Strings;",
            ),
            (
                "(ILcom/google/common/base/Strings;[JLcom/google/common/X;)Lcom/google/common/Y;",
                "(ILshaded/guava/base/Strings;[JLshaded/guava/X;)Lshaded/guava/Y;",
            ),
            (
                "Ljava/util/List<+Lcom/google/common/X<TT;>.Inner;>;",
                "Ljava/util/List<+Lshaded/guava/X<TT;>.Inner;>;",
            ),
            (
                "<T:Ljava/lang/Object;>(TT;)^Lcom/google/common/E;",
                "<T:Ljava/lang/Object;>(TT;)^Lshaded/guava/E;",
            ),
            // Class.forName and friends, and a resource path.
            (
                "com.google.common.base.Strings",
                "shaded.guava.base.Strings",
            ),
            ("[Lcom.google.common.X;", "[Lshaded.guava.X;"),
            (
                "/com/google/common/messages.properties",
                "/shaded/guava/messages.properties",
            ),
            // The package's own name, alone.
            ("com.google.common", "shaded.guava"),
        ] {
            assert_eq!(constant(&r, before), after, "{before}");
        }
    }

    #[test]
    fn only_the_package_and_what_is_under_it_is_relocated() {
        let r = guava();
        for untouched in [
            "com/google/commonx/Foo",
            "com/google/common",
            "Lcom/google/common;",
            "org/com/google/common/Foo",
            "Lorg/example/ZLcom/google/common/Foo;",
            "cannot load com.google.common.Foo",
            "Code",
            "com.google.commons.Foo",
        ] {
            assert_eq!(constant(&r, untouched), untouched, "{untouched}");
        }
    }

    #[test]
    fn the_most_specific_relocation_wins_whatever_the_order() {
        let r = relocator(&[
            ("com.google", "shaded.google", &[]),
            ("com.google.common", "shaded.guava", &[]),
        ]);
        assert_eq!(constant(&r, "com/google/common/X"), "shaded/guava/X");
        assert_eq!(
            constant(&r, "com/google/gson/Gson"),
            "shaded/google/gson/Gson"
        );
    }

    #[test]
    fn a_package_relocated_into_itself_is_rewritten_once() {
        let r = relocator(&[("com.foo", "com.foo.shaded", &[])]);
        assert_eq!(constant(&r, "Lcom/foo/X;"), "Lcom/foo/shaded/X;");
    }

    #[test]
    fn exclusions_leave_classes_and_packages_where_they_are() {
        let r = relocator(&[(
            "org.slf4j",
            "shaded.slf4j",
            &["org.slf4j.impl.*", "org.slf4j.Marker"],
        )]);
        assert_eq!(constant(&r, "org/slf4j/Logger"), "shaded/slf4j/Logger");
        assert_eq!(
            constant(&r, "Lorg/slf4j/impl/StaticLoggerBinder;"),
            "Lorg/slf4j/impl/StaticLoggerBinder;"
        );
        assert_eq!(
            constant(&r, "org.slf4j.impl.deep.X"),
            "org.slf4j.impl.deep.X"
        );
        assert_eq!(constant(&r, "org/slf4j/Marker"), "org/slf4j/Marker");
        assert_eq!(constant(&r, "org/slf4j/Marker$1"), "org/slf4j/Marker$1");
        assert_eq!(
            constant(&r, "org/slf4j/MarkerFactory"),
            "shaded/slf4j/MarkerFactory"
        );
    }

    #[test]
    fn entry_names_move_with_their_package() {
        let r = guava();
        assert_eq!(
            r.entry_name("com/google/common/base/Strings.class"),
            "shaded/guava/base/Strings.class"
        );
        assert_eq!(
            r.entry_name("com/google/common/base/messages.properties"),
            "shaded/guava/base/messages.properties"
        );
        assert_eq!(
            r.entry_name("META-INF/versions/11/com/google/common/X.class"),
            "META-INF/versions/11/shaded/guava/X.class"
        );
        assert_eq!(
            r.entry_name("META-INF/services/com.google.common.Spi"),
            "META-INF/services/shaded.guava.Spi"
        );
        for untouched in [
            "com/example/App.class",
            "META-INF/com/google/common/x.txt",
            "META-INF/services/java.sql.Driver",
            "com/google/common.properties",
        ] {
            assert_eq!(r.entry_name(untouched), untouched);
        }
    }

    #[test]
    fn registry_files_have_their_class_names_relocated() {
        let r = guava();
        let text = "# providers\ncom.google.common.Impl\norg.other.Impl\n\
                    k=com.google.common.A,com.google.common.B\n\
                    s=com/google/common/schema.xsd\nx.com.google.common.Y\n";
        assert_eq!(
            String::from_utf8(r.text(text.as_bytes().to_vec())).unwrap(),
            "# providers\nshaded.guava.Impl\norg.other.Impl\n\
             k=shaded.guava.A,shaded.guava.B\n\
             s=shaded/guava/schema.xsd\nx.com.google.common.Y\n"
        );
    }

    /// A class file by hand: magic, version, a pool, and a tail standing in for
    /// everything after it.
    fn class(pool: &[Vec<u8>], count: u16, tail: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61];
        bytes.extend_from_slice(&count.to_be_bytes());
        for entry in pool {
            bytes.extend_from_slice(entry);
        }
        bytes.extend_from_slice(tail);
        bytes
    }

    fn utf8(text: &str) -> Vec<u8> {
        let mut entry = vec![1];
        entry.extend_from_slice(&u16::try_from(text.len()).unwrap().to_be_bytes());
        entry.extend_from_slice(text.as_bytes());
        entry
    }

    #[test]
    fn a_class_files_pool_is_rewritten_and_its_tail_copied() {
        let r = guava();
        let long = vec![5, 0, 0, 0, 0, 0, 0, 0, 42];
        let before = class(
            &[
                utf8("com/google/common/base/Strings"),
                vec![7, 0, 1],
                long.clone(),
                utf8("(Lcom/google/common/X;)V"),
                vec![15, 6, 0, 3],
                utf8("Code"),
            ],
            // Six entries, the Long taking two slots, plus the unused zeroth.
            8,
            b"the rest of the class",
        );
        let after = r.class_file(before).unwrap();
        assert_eq!(
            after,
            class(
                &[
                    utf8("shaded/guava/base/Strings"),
                    vec![7, 0, 1],
                    long,
                    utf8("(Lshaded/guava/X;)V"),
                    vec![15, 6, 0, 3],
                    utf8("Code"),
                ],
                8,
                b"the rest of the class",
            )
        );
    }

    #[test]
    fn a_class_that_names_nothing_relocated_is_unchanged() {
        let bytes = class(&[utf8("com/example/App"), vec![7, 0, 1]], 3, b"tail");
        assert_eq!(guava().class_file(bytes.clone()).unwrap(), bytes);
    }

    #[test]
    fn what_is_not_a_class_file_is_refused() {
        let r = guava();
        let err = r.class_file(b"not a class".to_vec()).unwrap_err();
        assert!(err.contains("magic"), "{err}");
        let err = r
            .class_file(class(&[vec![1, 0, 40, b'x']], 2, b""))
            .unwrap_err();
        assert!(err.contains("truncated"), "{err}");
        let err = r.class_file(class(&[vec![2, 0, 0]], 2, b"")).unwrap_err();
        assert!(err.contains("tag 2"), "{err}");
    }

    #[test]
    fn a_name_that_would_outgrow_its_constant_is_refused() {
        let r = relocator(&[("a", &"b".repeat(200), &[])]);
        // Every `L` opens a class type, so every name is relocated.
        let text = "La/X;".repeat(13_000);
        let err = r.class_file(class(&[utf8(&text)], 2, b"")).unwrap_err();
        assert!(err.contains("longer than a constant"), "{err}");
    }

    #[test]
    fn no_relocations_change_nothing() {
        let r = Relocator::default();
        assert!(r.is_empty());
        assert_eq!(
            r.entry_name("com/google/common/X.class"),
            "com/google/common/X.class"
        );
        assert_eq!(r.class_file(b"anything".to_vec()).unwrap(), b"anything");
    }
}
