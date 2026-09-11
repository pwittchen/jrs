//! What one compile unit can see of another's classes: their API, not their
//! bytes. This is Gradle's compile avoidance, applied to the one boundary a
//! single module has (SPEC §7.2): the tests compile against `target/classes`,
//! and a main-source change that leaves those classes' API alone does not
//! recompile them.
//!
//! The digest keeps what `javac` reads from a class file when it compiles
//! other code against it: each class's name, flags, supertypes and nesting,
//! and for every member that is neither private nor synthetic its name,
//! descriptor, generic signature, annotations, declared exceptions and
//! constant value — `javac` inlines a `static final` constant, so its value
//! is API. Records and sealed classes keep their components and permitted
//! subclasses. Method bodies, private members, source-file names, nest
//! membership, and local, anonymous and private nested classes are left out.
//! Constant-pool indices are resolved to what they name, since a changed
//! body renumbers the pool.
//!
//! Some classes count by their bytes, because their API is more than their
//! signatures: Kotlin classes (an inline function's body is copied into its
//! callers), Scala classes (macros run at their callers' compile time, and a
//! `.tasty` file holds every body), `module-info`, and any class file jrs
//! cannot read. So do `.tasty` files and `META-INF/services` entries. And when
//! the classes include compile-time code — an annotation processor or a
//! Groovy AST transformation, which run while the tests compile — every class
//! counts by its bytes. Being conservative costs a recompile; being wrong
//! costs a test run against stale classes.
//!
//! [`class_info`] gives the same reading one class at a time, for
//! recompiling a unit file by file (`incremental.rs`): the class's API
//! digest, its constants alone, the source it came from and the classes it
//! refers to.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::Path;

use crate::error::{IoResultExt, Result};
use crate::project;
use crate::resolve::repo::sha256_hex;

const ACC_PRIVATE: u16 = 0x0002;
const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_MODULE: u16 = 0x8000;

/// Service files that register code a compiler runs while it compiles.
const COMPILER_SERVICES: [&str; 2] = [
    "META-INF/services/javax.annotation.processing.Processor",
    "META-INF/services/org.codehaus.groovy.transform.ASTTransformation",
];

/// The API of every class under `dir`, as a hex digest: equal for two trees
/// whose classes differ only in what another unit's compiler cannot see.
///
/// # Errors
///
/// [`crate::JrsError::Io`] if the tree or a file in it cannot be read. A class
/// file that cannot be parsed is not an error: it counts by its bytes.
pub fn api_digest(dir: &Path) -> Result<String> {
    let mut entries = Vec::new();
    let mut compiler_code = false;
    for path in project::find_all(dir)? {
        let relative = relative(dir, &path);
        let is_class = relative.ends_with(".class");
        if !is_class && !relative.ends_with(".tasty") && !relative.starts_with("META-INF/services/")
        {
            continue;
        }
        let bytes = std::fs::read(&path).path(&path)?;
        let api = if is_class { api(&bytes) } else { Api::Opaque };
        compiler_code |= api == Api::CompilerCode || COMPILER_SERVICES.contains(&relative.as_str());
        entries.push((relative, sha256_hex(&bytes), api));
    }

    let mut text = Vec::new();
    for (relative, hash, api) in entries {
        match api {
            Api::Visible(rendered) if !compiler_code => {
                put(&mut text, b"api");
                put(&mut text, relative.as_bytes());
                text.extend(rendered);
            }
            Api::Hidden if !compiler_code => continue,
            _ => {
                put(&mut text, b"bytes");
                put(&mut text, relative.as_bytes());
                put(&mut text, hash.as_bytes());
            }
        }
        text.push(b'\n');
    }
    Ok(sha256_hex(&text))
}

/// `com/example/Main.class`: `/`-separated on every OS, so the digest is too.
fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// What a class file contributes, from least to most conservative.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Fine,
    /// Nothing another unit can name: a local, anonymous, private nested or
    /// synthetic class.
    Hidden,
    /// Its bytes are its API.
    Opaque,
    /// It makes a compiler run code of the unit's own.
    CompilerCode,
}

#[derive(Debug, PartialEq, Eq)]
enum Api {
    Visible(Vec<u8>),
    Hidden,
    Opaque,
    CompilerCode,
}

fn api(bytes: &[u8]) -> Api {
    match parse(bytes) {
        Some(Parsed {
            verdict: Verdict::Fine,
            rendered,
            ..
        }) => Api::Visible(rendered),
        Some(Parsed {
            verdict: Verdict::Hidden,
            ..
        }) => Api::Hidden,
        Some(Parsed {
            verdict: Verdict::CompilerCode,
            ..
        }) => Api::CompilerCode,
        Some(_) | None => Api::Opaque,
    }
}

/// How another source can see a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A top-level class, or one jrs cannot read well enough to say.
    TopLevel,
    /// A member class another source can name.
    Nested,
    /// Nothing another source can name: a local, anonymous, private nested
    /// or synthetic class.
    Hidden,
}

/// One class file, as the incremental compiler needs it.
#[derive(Debug)]
pub(super) struct ClassInfo {
    /// `com/example/Main$Inner`.
    pub name: String,
    /// The `SourceFile` attribute: `Main.java`.
    pub source_file: Option<String>,
    pub kind: Kind,
    /// A digest of what [`api_digest`] keeps of the class; of its bytes when
    /// that is not enough; `-` for a hidden class.
    pub api: String,
    /// A digest of its visible compile-time constants alone, which `javac`
    /// copies into every class that reads them; `-` when it has none.
    pub constants: String,
    /// Every class name its constant pool mentions, the class's own too.
    pub refs: BTreeSet<String>,
}

/// Read one class file. `None` if it cannot be parsed.
pub(super) fn class_info(bytes: &[u8]) -> Option<ClassInfo> {
    let parsed = parse(bytes)?;
    let kind = match parsed.verdict {
        Verdict::Hidden => Kind::Hidden,
        Verdict::Fine if parsed.nested => Kind::Nested,
        _ => Kind::TopLevel,
    };
    let api = match parsed.verdict {
        Verdict::Fine => sha256_hex(&parsed.rendered),
        Verdict::Hidden => "-".to_string(),
        Verdict::Opaque | Verdict::CompilerCode => sha256_hex(bytes),
    };
    let constants = if kind == Kind::Hidden || parsed.constants.is_empty() {
        "-".to_string()
    } else {
        sha256_hex(&parsed.constants)
    };
    Some(ClassInfo {
        name: String::from_utf8_lossy(parsed.name).into_owned(),
        source_file: parsed
            .source_file
            .map(|s| String::from_utf8_lossy(s).into_owned()),
        kind,
        api,
        constants,
        refs: parsed.refs,
    })
}

/// A class name in each `L<name>;` or `L<name><` of a descriptor or
/// signature. A stray `L` in other text yields a name that matches no class,
/// which costs nothing; a candidate with a character no class name has is
/// passed over one byte at a time, so it cannot hide a real one.
fn descriptor_names(bytes: &[u8], out: &mut BTreeSet<String>) {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'L' {
            i += 1;
            continue;
        }
        let rest = &bytes[i + 1..];
        match rest.iter().position(|&b| b == b';' || b == b'<') {
            Some(end) if end > 0 && !rest[..end].iter().any(|b| b":()[].>; ".contains(b)) => {
                out.insert(String::from_utf8_lossy(&rest[..end]).into_owned());
                i += end + 2;
            }
            _ => i += 1,
        }
    }
}

/// A length-prefixed field, so no two renderings run together.
fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

fn num(out: &mut Vec<u8>, n: impl std::fmt::Display) {
    put(out, n.to_string().as_bytes());
}

fn raise(verdict: &mut Verdict, to: Verdict) {
    if to > *verdict {
        *verdict = to;
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u1(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u2(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u4(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}

/// A constant-pool entry, as far as the API needs one.
#[derive(Clone, Copy)]
enum Const<'a> {
    /// Slot 0, the second half of a long or double, or an entry the API
    /// never refers to.
    Other,
    Utf8(&'a [u8]),
    Int(u32),
    Float(u32),
    Long(u64),
    Double(u64),
    Class(u16),
    Str(u16),
}

fn constant_pool<'a>(r: &mut Reader<'a>) -> Option<Vec<Const<'a>>> {
    let count = usize::from(r.u2()?);
    let mut pool = vec![Const::Other];
    while pool.len() < count {
        let tag = r.u1()?;
        let entry = match tag {
            1 => {
                let n = r.u2()?;
                Const::Utf8(r.take(usize::from(n))?)
            }
            3 => Const::Int(r.u4()?),
            4 => Const::Float(r.u4()?),
            5 | 6 => {
                let value = (u64::from(r.u4()?) << 32) | u64::from(r.u4()?);
                pool.push(if tag == 5 {
                    Const::Long(value)
                } else {
                    Const::Double(value)
                });
                // A long or a double takes two slots.
                Const::Other
            }
            7 => Const::Class(r.u2()?),
            8 => Const::Str(r.u2()?),
            // MethodType, Module, Package.
            16 | 19 | 20 => {
                r.u2()?;
                Const::Other
            }
            // Field/Method/InterfaceMethod refs, NameAndType, Dynamic,
            // InvokeDynamic.
            9..=12 | 17 | 18 => {
                r.u4()?;
                Const::Other
            }
            // MethodHandle.
            15 => {
                r.take(3)?;
                Const::Other
            }
            _ => return None,
        };
        pool.push(entry);
    }
    Some(pool)
}

/// What one class file holds, read once for both [`api_digest`] and
/// [`class_info`].
struct Parsed<'a> {
    verdict: Verdict,
    rendered: Vec<u8>,
    /// The visible fields with a `ConstantValue`, rendered as in `rendered`.
    constants: Vec<u8>,
    name: &'a [u8],
    source_file: Option<&'a [u8]>,
    /// Whether the class has an `InnerClasses` entry of its own.
    nested: bool,
    refs: BTreeSet<String>,
}

fn parse(bytes: &[u8]) -> Option<Parsed<'_>> {
    let mut r = Reader::new(bytes);
    if r.u4()? != 0xCAFE_BABE {
        return None;
    }
    let _minor = r.u2()?;
    let major = r.u2()?;
    let pool = constant_pool(&mut r)?;
    let access = r.u2()?;
    let this = r.u2()?;
    let superclass = r.u2()?;
    let class = Class {
        pool,
        this,
        source_file: Cell::new(None),
        nested: Cell::new(false),
        constant: Cell::new(false),
    };
    let name = class.class_name(this)?;
    if access & ACC_MODULE != 0 {
        return Some(Parsed {
            verdict: Verdict::Opaque,
            rendered: Vec::new(),
            constants: Vec::new(),
            name,
            source_file: None,
            nested: false,
            refs: BTreeSet::new(),
        });
    }

    let mut out = Vec::new();
    let mut constants = Vec::new();
    let mut verdict = Verdict::Fine;
    put(&mut out, b"class");
    num(&mut out, major);
    num(&mut out, access);
    put(&mut out, name);
    put(
        &mut out,
        if superclass == 0 {
            b""
        } else {
            class.class_name(superclass)?
        },
    );
    let interfaces = r.u2()?;
    num(&mut out, interfaces);
    for _ in 0..interfaces {
        put(&mut out, class.class_name(r.u2()?)?);
    }

    for kind in [&b"field"[..], &b"method"[..]] {
        let count = r.u2()?;
        let mut members = Vec::new();
        let mut constant_members = Vec::new();
        for _ in 0..count {
            let flags = r.u2()?;
            let name = class.utf8(r.u2()?)?;
            let descriptor = class.utf8(r.u2()?)?;
            let mut member = Vec::new();
            put(&mut member, kind);
            num(&mut member, flags);
            put(&mut member, name);
            put(&mut member, descriptor);
            // A private member's attributes are read past, and cannot make
            // the class opaque: nothing outside it sees them.
            let mut own = Verdict::Fine;
            class.constant.set(false);
            class.attributes(&mut r, &mut member, &mut own)?;
            if flags & (ACC_PRIVATE | ACC_SYNTHETIC) == 0 {
                raise(&mut verdict, own);
                if class.constant.get() {
                    constant_members.push(member.clone());
                }
                members.push(member);
            }
        }
        // Declaration order is not API: moving a method changes nothing a
        // caller compiles against.
        members.sort();
        for member in members {
            out.extend(member);
        }
        constant_members.sort();
        for member in constant_members {
            constants.extend(member);
        }
    }
    class.attributes(&mut r, &mut out, &mut verdict)?;
    if !r.done() {
        return None;
    }
    if access & ACC_SYNTHETIC != 0 {
        verdict = Verdict::Hidden;
    }
    let refs = class.refs();
    Some(Parsed {
        verdict,
        rendered: out,
        constants,
        name,
        source_file: class.source_file.get().and_then(|i| class.utf8(i)),
        nested: class.nested.get(),
        refs,
    })
}

struct Class<'a> {
    pool: Vec<Const<'a>>,
    this: u16,
    /// Found on the way through the attributes.
    source_file: Cell<Option<u16>>,
    nested: Cell<bool>,
    /// Whether the member being read has a `ConstantValue`.
    constant: Cell<bool>,
}

impl<'a> Class<'a> {
    /// Every class the pool names: its `Class` entries, and the classes in
    /// every descriptor and signature, which name types the class uses
    /// without a `Class` entry for them.
    fn refs(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for entry in &self.pool {
            match *entry {
                Const::Class(name) => match self.utf8(name) {
                    Some(array) if array.starts_with(b"[") => descriptor_names(array, &mut out),
                    Some(name) => {
                        out.insert(String::from_utf8_lossy(name).into_owned());
                    }
                    None => {}
                },
                Const::Utf8(bytes) => descriptor_names(bytes, &mut out),
                _ => {}
            }
        }
        out
    }

    fn utf8(&self, index: u16) -> Option<&'a [u8]> {
        match self.pool.get(usize::from(index))? {
            Const::Utf8(bytes) => Some(bytes),
            _ => None,
        }
    }

    fn class_name(&self, index: u16) -> Option<&'a [u8]> {
        match self.pool.get(usize::from(index))? {
            Const::Class(name) => self.utf8(*name),
            _ => None,
        }
    }

    /// A constant value, typed, whichever kind of entry holds it.
    fn constant(&self, index: u16, out: &mut Vec<u8>) -> Option<()> {
        match *self.pool.get(usize::from(index))? {
            Const::Int(v) => num(out, format_args!("I{v}")),
            Const::Float(v) => num(out, format_args!("F{v:08x}")),
            Const::Long(v) => num(out, format_args!("J{v}")),
            Const::Double(v) => num(out, format_args!("D{v:016x}")),
            Const::Utf8(bytes) => {
                put(out, b"U");
                put(out, bytes);
            }
            Const::Str(utf8) => {
                put(out, b"S");
                put(out, self.utf8(utf8)?);
            }
            Const::Class(name) => {
                put(out, b"C");
                put(out, self.utf8(name)?);
            }
            Const::Other => return None,
        }
        Some(())
    }

    /// Render an `attributes` table, keeping what is API and reading past
    /// the rest. An attribute jrs does not know makes the class opaque.
    fn attributes(
        &self,
        r: &mut Reader<'a>,
        out: &mut Vec<u8>,
        verdict: &mut Verdict,
    ) -> Option<()> {
        let count = r.u2()?;
        for _ in 0..count {
            let name = self.utf8(r.u2()?)?;
            let length = r.u4()?;
            let mut body = Reader::new(r.take(usize::try_from(length).ok()?)?);
            let b = &mut body;
            match name {
                // Bodies, debugging information, and what only the JVM
                // reads: none of it is seen by a compiler reading the class.
                b"SourceFile" => {
                    self.source_file.set(Some(b.u2()?));
                    continue;
                }
                b"Code"
                | b"SourceDebugExtension"
                | b"NestHost"
                | b"NestMembers"
                | b"BootstrapMethods"
                | b"EnclosingMethod"
                | b"Synthetic"
                | b"LineNumberTable"
                | b"LocalVariableTable"
                | b"LocalVariableTypeTable"
                | b"StackMapTable" => continue,
                b"Deprecated" => put(out, b"Deprecated"),
                b"Signature" => {
                    put(out, b"Signature");
                    put(out, self.utf8(b.u2()?)?);
                }
                b"ConstantValue" => {
                    self.constant.set(true);
                    put(out, b"ConstantValue");
                    self.constant(b.u2()?, out)?;
                }
                b"Exceptions" | b"PermittedSubclasses" => {
                    put(out, name);
                    let n = b.u2()?;
                    num(out, n);
                    for _ in 0..n {
                        put(out, self.class_name(b.u2()?)?);
                    }
                }
                b"MethodParameters" => {
                    put(out, name);
                    let n = b.u1()?;
                    num(out, n);
                    for _ in 0..n {
                        let parameter = b.u2()?;
                        put(
                            out,
                            if parameter == 0 {
                                b""
                            } else {
                                self.utf8(parameter)?
                            },
                        );
                        num(out, b.u2()?);
                    }
                }
                b"RuntimeVisibleAnnotations" | b"RuntimeInvisibleAnnotations" => {
                    put(out, name);
                    let n = b.u2()?;
                    num(out, n);
                    for _ in 0..n {
                        self.annotation(b, out, verdict)?;
                    }
                }
                b"RuntimeVisibleParameterAnnotations" | b"RuntimeInvisibleParameterAnnotations" => {
                    put(out, name);
                    let parameters = b.u1()?;
                    num(out, parameters);
                    for _ in 0..parameters {
                        let n = b.u2()?;
                        num(out, n);
                        for _ in 0..n {
                            self.annotation(b, out, verdict)?;
                        }
                    }
                }
                b"RuntimeVisibleTypeAnnotations" | b"RuntimeInvisibleTypeAnnotations" => {
                    put(out, name);
                    let n = b.u2()?;
                    num(out, n);
                    for _ in 0..n {
                        self.type_annotation(b, out, verdict)?;
                    }
                }
                b"AnnotationDefault" => {
                    put(out, name);
                    self.element(b, out, verdict)?;
                }
                b"InnerClasses" => self.inner_classes(b, out, verdict)?,
                b"Record" => {
                    put(out, name);
                    let n = b.u2()?;
                    num(out, n);
                    for _ in 0..n {
                        put(out, self.utf8(b.u2()?)?);
                        put(out, self.utf8(b.u2()?)?);
                        self.attributes(b, out, verdict)?;
                    }
                }
                // Kotlin and Scala write their own attributes, and module
                // descriptors are not classes; anything else is unknown.
                _ => {
                    raise(verdict, Verdict::Opaque);
                    continue;
                }
            }
            if !body.done() {
                return None;
            }
        }
        Some(())
    }

    /// The class's own nesting: its own entry, which also says whether it
    /// can be named from outside at all, and its member classes. Entries for
    /// other classes' nested classes follow what the bodies use, so they are
    /// left out. Classes are compared by name, since a pool may hold two
    /// entries for one class.
    fn inner_classes(
        &self,
        b: &mut Reader<'a>,
        out: &mut Vec<u8>,
        verdict: &mut Verdict,
    ) -> Option<()> {
        let this = self.class_name(self.this)?;
        let n = b.u2()?;
        let mut entries = Vec::new();
        for _ in 0..n {
            let inner = b.u2()?;
            let outer = b.u2()?;
            let simple_name = b.u2()?;
            let flags = b.u2()?;
            let own = self.class_name(inner)? == this;
            if own {
                self.nested.set(true);
                if outer == 0 || flags & ACC_PRIVATE != 0 {
                    raise(verdict, Verdict::Hidden);
                }
            }
            let member = outer != 0
                && self.class_name(outer)? == this
                && flags & (ACC_PRIVATE | ACC_SYNTHETIC) == 0;
            if !own && !member {
                continue;
            }
            let mut entry = Vec::new();
            put(&mut entry, self.class_name(inner)?);
            put(
                &mut entry,
                if outer == 0 {
                    b""
                } else {
                    self.class_name(outer)?
                },
            );
            put(
                &mut entry,
                if simple_name == 0 {
                    b""
                } else {
                    self.utf8(simple_name)?
                },
            );
            num(&mut entry, flags);
            entries.push(entry);
        }
        // A table left with nothing reads as no table: a body that starts
        // using a lambda or an anonymous class adds one.
        if entries.is_empty() {
            return Some(());
        }
        entries.sort();
        put(out, b"InnerClasses");
        num(out, entries.len());
        for entry in entries {
            out.extend(entry);
        }
        Some(())
    }

    fn annotation(
        &self,
        b: &mut Reader<'a>,
        out: &mut Vec<u8>,
        verdict: &mut Verdict,
    ) -> Option<()> {
        let kind = self.utf8(b.u2()?)?;
        match kind {
            b"Lkotlin/Metadata;"
            | b"Lscala/reflect/ScalaSignature;"
            | b"Lscala/reflect/ScalaLongSignature;" => raise(verdict, Verdict::Opaque),
            b"Lorg/codehaus/groovy/transform/GroovyASTTransformationClass;" => {
                raise(verdict, Verdict::CompilerCode);
            }
            _ => {}
        }
        put(out, kind);
        let pairs = b.u2()?;
        num(out, pairs);
        for _ in 0..pairs {
            put(out, self.utf8(b.u2()?)?);
            self.element(b, out, verdict)?;
        }
        Some(())
    }

    fn element(&self, b: &mut Reader<'a>, out: &mut Vec<u8>, verdict: &mut Verdict) -> Option<()> {
        let tag = b.u1()?;
        out.push(tag);
        match tag {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => {
                self.constant(b.u2()?, out)?;
            }
            b'e' => {
                put(out, self.utf8(b.u2()?)?);
                put(out, self.utf8(b.u2()?)?);
            }
            b'c' => put(out, self.utf8(b.u2()?)?),
            b'@' => self.annotation(b, out, verdict)?,
            b'[' => {
                let n = b.u2()?;
                num(out, n);
                for _ in 0..n {
                    self.element(b, out, verdict)?;
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// A type annotation: where in the type it sits, then the annotation.
    fn type_annotation(
        &self,
        b: &mut Reader<'a>,
        out: &mut Vec<u8>,
        verdict: &mut Verdict,
    ) -> Option<()> {
        let target = b.u1()?;
        out.push(target);
        match target {
            // A type parameter, a formal parameter.
            0x00 | 0x01 | 0x16 => out.push(b.u1()?),
            // A supertype, a thrown type.
            0x10 | 0x17 => num(out, b.u2()?),
            // A type parameter's bound.
            0x11 | 0x12 => {
                out.push(b.u1()?);
                out.push(b.u1()?);
            }
            // A field's type, a method's return or receiver type.
            0x13..=0x15 => {}
            // The rest only occur inside a method body.
            0x40 | 0x41 => {
                let n = b.u2()?;
                b.take(usize::from(n) * 6)?;
            }
            0x42..=0x46 => {
                b.u2()?;
            }
            0x47..=0x4B => {
                b.u2()?;
                b.u1()?;
            }
            _ => return None,
        }
        let steps = b.u1()?;
        out.push(steps);
        out.extend_from_slice(b.take(usize::from(steps) * 2)?);
        self.annotation(b, out, verdict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Builds just enough of a class file to exercise the digest. The
    /// constant pool is filled in the order entries are asked for, so two
    /// classes with the same API can still have different pools.
    #[derive(Default)]
    struct ClassFile {
        pool: Vec<Vec<u8>>,
        slots: u16,
    }

    struct Member<'a> {
        flags: u16,
        name: &'a str,
        descriptor: &'a str,
        attributes: Vec<(&'a str, Vec<u8>)>,
    }

    fn member<'a>(flags: u16, name: &'a str, descriptor: &'a str) -> Member<'a> {
        Member {
            flags,
            name,
            descriptor,
            attributes: Vec::new(),
        }
    }

    impl ClassFile {
        fn entry(&mut self, bytes: Vec<u8>, slots: u16) -> u16 {
            self.pool.push(bytes);
            self.slots += slots;
            self.slots - slots + 1
        }

        fn utf8(&mut self, s: &str) -> u16 {
            let mut bytes = vec![1];
            bytes.extend((s.len() as u16).to_be_bytes());
            bytes.extend(s.as_bytes());
            self.entry(bytes, 1)
        }

        fn class(&mut self, name: &str) -> u16 {
            let name = self.utf8(name);
            let mut bytes = vec![7];
            bytes.extend(name.to_be_bytes());
            self.entry(bytes, 1)
        }

        fn int(&mut self, value: i32) -> u16 {
            let mut bytes = vec![3];
            bytes.extend(value.to_be_bytes());
            self.entry(bytes, 1)
        }

        fn long(&mut self, value: i64) -> u16 {
            let mut bytes = vec![5];
            bytes.extend(value.to_be_bytes());
            self.entry(bytes, 2)
        }

        fn u2(v: u16) -> Vec<u8> {
            v.to_be_bytes().to_vec()
        }

        fn attributes(&mut self, attributes: &[(&str, Vec<u8>)]) -> Vec<u8> {
            let mut out = Self::u2(attributes.len() as u16);
            for (name, body) in attributes {
                out.extend(Self::u2(self.utf8(name)));
                out.extend((body.len() as u32).to_be_bytes());
                out.extend(body);
            }
            out
        }

        fn build(
            mut self,
            flags: u16,
            name: &str,
            fields: &[Member<'_>],
            methods: &[Member<'_>],
            attributes: &[(&str, Vec<u8>)],
        ) -> Vec<u8> {
            let this = self.class(name);
            let superclass = self.class("java/lang/Object");
            let mut body = Vec::new();
            body.extend(Self::u2(flags));
            body.extend(Self::u2(this));
            body.extend(Self::u2(superclass));
            body.extend(Self::u2(0));
            for members in [fields, methods] {
                body.extend(Self::u2(members.len() as u16));
                for m in members {
                    body.extend(Self::u2(m.flags));
                    body.extend(Self::u2(self.utf8(m.name)));
                    body.extend(Self::u2(self.utf8(m.descriptor)));
                    body.extend(self.attributes(&m.attributes));
                }
            }
            body.extend(self.attributes(attributes));

            let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
            out.extend(Self::u2(self.slots + 1));
            for entry in &self.pool {
                out.extend(entry);
            }
            out.extend(body);
            out
        }
    }

    const PUBLIC: u16 = 0x0001;
    const STATIC_FINAL: u16 = 0x0008 | 0x0010;

    /// `public class Calc` with `add(II)I`, whose body is `code`, a
    /// `LIMIT` constant, and whatever `extra` methods.
    fn calc(code: &[u8], limit: i32, extra: &[Member<'_>], junk: usize) -> Vec<u8> {
        let mut c = ClassFile::default();
        // Unrelated entries first, the way a changed body renumbers a pool.
        for i in 0..junk {
            c.utf8(&format!("junk{i}"));
        }
        let limit_value = c.int(limit);
        let mut add = member(PUBLIC | 0x0008, "add", "(II)I");
        add.attributes.push(("Code", code.to_vec()));
        let mut methods = vec![add];
        methods.extend(extra.iter().map(|m| Member {
            flags: m.flags,
            name: m.name,
            descriptor: m.descriptor,
            attributes: m.attributes.clone(),
        }));
        let mut limit_field = member(PUBLIC | STATIC_FINAL, "LIMIT", "I");
        limit_field
            .attributes
            .push(("ConstantValue", ClassFile::u2(limit_value)));
        c.build(
            PUBLIC | 0x0020,
            "com/example/Calc",
            &[limit_field],
            &methods,
            &[("SourceFile", ClassFile::u2(1))],
        )
    }

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let root = std::env::temp_dir().join(format!("jrs-abi-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree(root)
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn digest(&self) -> String {
            api_digest(&self.0).unwrap()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_changed_body_is_not_a_changed_api() {
        let tree = Tree::new("body");
        tree.write("com/example/Calc.class", &calc(&[1, 2, 3], 10, &[], 0));
        let before = tree.digest();
        tree.write("com/example/Calc.class", &calc(&[9, 9, 9, 9], 10, &[], 3));
        assert_eq!(tree.digest(), before, "a new body and a renumbered pool");
    }

    #[test]
    fn private_and_synthetic_members_are_not_api_but_new_public_ones_are() {
        let tree = Tree::new("members");
        tree.write("com/example/Calc.class", &calc(&[1], 10, &[], 0));
        let before = tree.digest();
        let hidden = [
            member(0x0002, "helper", "()V"),
            member(0x1008, "lambda$add$0", "()V"),
        ];
        tree.write("com/example/Calc.class", &calc(&[1], 10, &hidden, 0));
        assert_eq!(tree.digest(), before);

        let visible = [member(PUBLIC, "subtract", "(II)I")];
        tree.write("com/example/Calc.class", &calc(&[1], 10, &visible, 0));
        assert_ne!(tree.digest(), before, "a public method is API");
        // Package-private is API too: tests usually share the package.
        let package = [member(0, "subtract", "(II)I")];
        tree.write("com/example/Calc.class", &calc(&[1], 10, &package, 0));
        assert_ne!(tree.digest(), before, "a package-private method is API");
    }

    #[test]
    fn a_constant_is_api_because_javac_inlines_it() {
        let tree = Tree::new("constant");
        tree.write("com/example/Calc.class", &calc(&[1], 10, &[], 0));
        let before = tree.digest();
        tree.write("com/example/Calc.class", &calc(&[1], 11, &[], 0));
        assert_ne!(tree.digest(), before);
    }

    #[test]
    fn member_order_is_not_api() {
        let tree = Tree::new("order");
        let a = [member(PUBLIC, "a", "()V"), member(PUBLIC, "b", "()V")];
        let b = [member(PUBLIC, "b", "()V"), member(PUBLIC, "a", "()V")];
        tree.write("com/example/Calc.class", &calc(&[1], 10, &a, 0));
        let before = tree.digest();
        tree.write("com/example/Calc.class", &calc(&[1], 10, &b, 0));
        assert_eq!(tree.digest(), before);
    }

    /// An anonymous class: its own `InnerClasses` entry names no outer class.
    /// The long constant makes its pool hold a two-slot entry.
    fn anonymous(name: &str) -> Vec<u8> {
        let mut c = ClassFile::default();
        c.long(7);
        let this = c.class(name);
        let mut inner = ClassFile::u2(1);
        inner.extend(ClassFile::u2(this));
        inner.extend(ClassFile::u2(0));
        inner.extend(ClassFile::u2(0));
        inner.extend(ClassFile::u2(0));
        c.build(0x0020, name, &[], &[], &[("InnerClasses", inner)])
    }

    #[test]
    fn anonymous_classes_are_not_api() {
        let tree = Tree::new("anonymous");
        tree.write("com/example/Calc.class", &calc(&[1], 10, &[], 0));
        let before = tree.digest();
        tree.write("com/example/Calc$1.class", &anonymous("com/example/Calc$1"));
        assert_eq!(tree.digest(), before);
    }

    #[test]
    fn a_kotlin_class_counts_by_its_bytes() {
        let kotlin = |code: &[u8]| {
            let mut c = ClassFile::default();
            let kind = c.utf8("Lkotlin/Metadata;");
            let mut annotations = ClassFile::u2(1);
            annotations.extend(ClassFile::u2(kind));
            annotations.extend(ClassFile::u2(0));
            let mut inline = member(PUBLIC | 0x0008, "twice", "(I)I");
            inline.attributes.push(("Code", code.to_vec()));
            c.build(
                PUBLIC | 0x0020,
                "com/example/UtilKt",
                &[],
                &[inline],
                &[("RuntimeVisibleAnnotations", annotations)],
            )
        };
        let tree = Tree::new("kotlin");
        tree.write("com/example/UtilKt.class", &kotlin(&[1]));
        let before = tree.digest();
        tree.write("com/example/UtilKt.class", &kotlin(&[2]));
        assert_ne!(
            tree.digest(),
            before,
            "an inline function's body is copied into its callers"
        );
    }

    #[test]
    fn an_unknown_attribute_or_an_unreadable_class_counts_by_its_bytes() {
        let tree = Tree::new("opaque");
        tree.write("com/example/Broken.class", b"not a class file");
        let before = tree.digest();
        tree.write("com/example/Broken.class", b"not a class file either");
        assert_ne!(tree.digest(), before);

        let odd = |body: Vec<u8>| {
            ClassFile::default().build(PUBLIC, "com/example/Odd", &[], &[], &[("Mystery", body)])
        };
        tree.write("com/example/Odd.class", &odd(vec![1]));
        let before = tree.digest();
        tree.write("com/example/Odd.class", &odd(vec![2]));
        assert_ne!(tree.digest(), before);
    }

    #[test]
    fn an_annotation_processor_makes_every_class_count_by_its_bytes() {
        let tree = Tree::new("processor");
        tree.write(
            "META-INF/services/javax.annotation.processing.Processor",
            b"com.example.Gen\n",
        );
        tree.write("com/example/Calc.class", &calc(&[1], 10, &[], 0));
        let before = tree.digest();
        tree.write("com/example/Calc.class", &calc(&[2], 10, &[], 0));
        assert_ne!(tree.digest(), before, "the processor runs this code");
    }

    #[test]
    fn resources_are_not_api_but_tasty_files_are() {
        let tree = Tree::new("resources");
        tree.write("com/example/Calc.class", &calc(&[1], 10, &[], 0));
        tree.write("app.properties", b"a=1");
        let before = tree.digest();
        tree.write("app.properties", b"a=2");
        assert_eq!(tree.digest(), before);

        tree.write("com/example/Main.tasty", b"one");
        let before = tree.digest();
        tree.write("com/example/Main.tasty", b"two");
        assert_ne!(tree.digest(), before);
    }

    #[test]
    fn descriptors_and_signatures_name_their_classes() {
        let names = |text: &str| {
            let mut out = BTreeSet::new();
            descriptor_names(text.as_bytes(), &mut out);
            out.into_iter().collect::<Vec<_>>()
        };
        assert_eq!(names("(ILp/A;[Lp/B;)Lp/C;"), ["p/A", "p/B", "p/C"]);
        assert_eq!(
            names("Ljava/util/Map<Lp/K;Lp/V;>;"),
            ["java/util/Map", "p/K", "p/V"]
        );
        // A type parameter whose name holds an `L` cannot hide its bound.
        assert!(names("<ELEM:Lp/Base;>Ljava/lang/Object;").contains(&"p/Base".to_string()));
        assert!(names("hello").is_empty());
    }

    #[test]
    fn class_info_names_the_source_and_the_constants() {
        let info = class_info(&calc(&[1], 10, &[], 0)).unwrap();
        assert_eq!(info.name, "com/example/Calc");
        assert_eq!(info.kind, Kind::TopLevel);
        assert_ne!(info.constants, "-");
        assert!(info.refs.contains("java/lang/Object"));
        let body = class_info(&calc(&[2, 3], 10, &[], 2)).unwrap();
        assert_eq!(
            (body.api, body.constants),
            (info.api, info.constants.clone())
        );
        assert_ne!(
            class_info(&calc(&[1], 11, &[], 0)).unwrap().constants,
            info.constants
        );
        assert_eq!(
            class_info(&anonymous("com/example/Calc$1")).unwrap().kind,
            Kind::Hidden
        );
    }

    #[test]
    fn a_missing_directory_has_an_empty_api() {
        let tree = Tree::new("missing");
        assert_eq!(
            api_digest(&tree.0.join("nope")).unwrap(),
            api_digest(&tree.0).unwrap()
        );
    }
}
