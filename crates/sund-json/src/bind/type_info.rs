//! Type metadata for schema-driven unmarshal.

/// Type kinds for bind fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float32,
    Float64,
    /// `*const c_char` style NUL-terminated string.
    String,
    /// String with an explicit length companion field.
    StringLen,
    /// Nested struct by value.
    Struct,
    /// Nested struct pointer (arena allocated).
    StructPtr,
    /// `T* + usize len` array. Element kind in `Field::elem_kind`.
    Array,
}

/// Descriptor for a single struct field.
#[derive(Clone, Debug)]
pub struct Field {
    /// JSON key name.
    pub name: &'static str,
    /// What type of value this field holds.
    pub kind: Kind,
    /// Byte offset of the primary field within the struct.
    pub offset: usize,
    /// Byte offset of the length companion (for `Array` / `StringLen`).
    pub len_offset: usize,
    /// Element kind for `Array`; ignored for other kinds.
    pub elem_kind: Option<Kind>,
    /// Type descriptor for `Struct` / `StructPtr` / `Array`-of-struct.
    pub elem_type: Option<&'static TypeInfo>,
}

/// Type descriptor for a struct.
#[derive(Clone, Debug)]
pub struct TypeInfo {
    /// Type name (for diagnostics).
    pub name: &'static str,
    /// `std::mem::size_of::<T>()`.
    pub size: usize,
    /// Fields of the struct.
    pub fields: &'static [Field],
}

impl Kind {
    /// Size in bytes when laid out as an array element.
    /// Returns `None` for kinds that cannot be array elements.
    pub fn elem_size(&self, elem_type: Option<&TypeInfo>) -> Option<usize> {
        match self {
            Kind::Bool => Some(1),
            Kind::Int8 | Kind::Uint8 => Some(1),
            Kind::Int16 | Kind::Uint16 => Some(2),
            Kind::Int32 | Kind::Uint32 => Some(4),
            Kind::Int64 | Kind::Uint64 => Some(8),
            Kind::Float32 => Some(4),
            Kind::Float64 => Some(8),
            Kind::String => Some(std::mem::size_of::<*const u8>()),
            Kind::Struct => elem_type.map(|t| t.size),
            Kind::StructPtr => Some(std::mem::size_of::<*const u8>()),
            Kind::StringLen | Kind::Array => None,
        }
    }
}
