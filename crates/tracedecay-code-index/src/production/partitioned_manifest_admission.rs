//! Admission around the canonical manifest's existing Serde implementation.
//!
//! No wire fields or validation rules live here. Scalar payloads and JSON's
//! unescape scratch are admitted before decoding starts. The adapter admits
//! actual element bodies before a collection can grow, suppressing size hints
//! that would let a visitor allocate before its elements reach admission.

use super::super::CodeIndexProductionErrorV1;
use serde::Deserialize;
use serde::de::{
    DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use std::cell::RefCell;
use std::mem::{align_of, size_of};

struct Admission<'a> {
    admit: &'a mut dyn FnMut(usize) -> Result<(), CodeIndexProductionErrorV1>,
    required: usize,
    failure: Option<CodeIndexProductionErrorV1>,
}

impl Admission<'_> {
    fn grow<E: serde::de::Error>(&mut self, additional: usize) -> Result<(), E> {
        if additional == 0 {
            return Ok(());
        }
        let Some(required) = self.required.checked_add(additional) else {
            return self.refuse(CodeIndexProductionErrorV1::Contract(
                "manifest decode allocation size overflowed".to_owned(),
            ));
        };
        if let Err(error) = (self.admit)(required) {
            return self.refuse(error);
        }
        self.required = required;
        Ok(())
    }

    fn refuse<E: serde::de::Error>(
        &mut self,
        failure: CodeIndexProductionErrorV1,
    ) -> Result<(), E> {
        self.failure = Some(failure);
        Err(E::custom("manifest decode allocation was not admitted"))
    }
}

type AdmissionRef<'a, 'b> = &'a RefCell<Admission<'b>>;

fn overflow<E: serde::de::Error>(admission: AdmissionRef<'_, '_>) -> E {
    admission.borrow_mut().failure = Some(CodeIndexProductionErrorV1::Contract(
        "manifest decode collection capacity overflowed".to_owned(),
    ));
    E::custom("manifest decode collection capacity overflowed")
}

/// RawVec's pinned minimum and geometric growth, including old/new storage
/// while growth is in progress. Slots are separate from scalar payloads.
fn vector_peak(entries: usize, element_bytes: usize) -> Option<usize> {
    if entries == 0 || element_bytes == 0 {
        return Some(0);
    }
    let minimum = if element_bytes == 1 {
        8
    } else if element_bytes <= 1024 {
        4
    } else {
        1
    };
    let capacity = entries.max(minimum).checked_next_power_of_two()?;
    let old_capacity = if capacity > minimum { capacity / 2 } else { 0 };
    capacity
        .checked_add(old_capacity)?
        .checked_mul(element_bytes)
}

#[derive(Default)]
struct SequenceCharge {
    entries: usize,
    bytes: usize,
}

impl SequenceCharge {
    fn element<T, E: serde::de::Error>(
        &mut self,
        admission: AdmissionRef<'_, '_>,
    ) -> Result<(), E> {
        let entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| overflow(admission))?;
        let bytes = vector_peak(entries, size_of::<T>()).ok_or_else(|| overflow(admission))?;
        admission
            .borrow_mut()
            .grow(bytes.saturating_sub(self.bytes))?;
        self.entries = entries;
        self.bytes = bytes;
        Ok(())
    }
}

#[derive(Default)]
struct MapCharge {
    entries: usize,
    key_bytes: usize,
    key_align: usize,
    bytes: usize,
}

impl MapCharge {
    fn key<T>(&mut self) {
        self.key_bytes = size_of::<T>();
        self.key_align = align_of::<T>();
    }

    fn value<T, E: serde::de::Error>(&mut self, admission: AdmissionRef<'_, '_>) -> Result<(), E> {
        let entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| overflow(admission))?;
        let align = self.key_align.max(align_of::<T>());
        let pair = self
            .key_bytes
            .checked_next_multiple_of(align_of::<T>())
            .and_then(|bytes| bytes.checked_add(size_of::<T>()))
            .and_then(|bytes| bytes.checked_next_multiple_of(align))
            .ok_or_else(|| overflow(admission))?;
        // A map visitor may collect Serde's enum-content Vec, a BTreeMap,
        // or a HashMap. Structs skip this because their body is already counted.
        let vector = vector_peak(entries, pair).ok_or_else(|| overflow(admission))?;
        let tree = super::super::resident_bytes::btree_bytes(entries, pair);
        // Sixteen buckets cover the SwissTable minimum classes. The next
        // power-of-two above 8/7 entries covers its load factor; three tables
        // bound the preceding and replacement storage during a grow.
        let buckets = entries
            .checked_add(1)
            .and_then(|n| n.checked_mul(8))
            .map(|n| n / 7)
            .and_then(usize::checked_next_power_of_two)
            .map(|n| n.max(16))
            .ok_or_else(|| overflow(admission))?;
        let table = buckets
            .checked_mul(pair)
            .and_then(|bytes| bytes.checked_next_multiple_of(align.max(16)))
            .and_then(|bytes| bytes.checked_add(buckets))
            .and_then(|bytes| bytes.checked_add(16))
            .and_then(|bytes| bytes.checked_mul(3))
            .ok_or_else(|| overflow(admission))?;
        let bytes = vector.max(tree).max(table);
        admission
            .borrow_mut()
            .grow(bytes.saturating_sub(self.bytes))?;
        self.entries = entries;
        self.bytes = bytes;
        Ok(())
    }
}

struct Admitted<'a, 'b, D> {
    inner: D,
    admission: AdmissionRef<'a, 'b>,
}
struct AdmittedVisitor<'a, 'b, V> {
    inner: V,
    admission: AdmissionRef<'a, 'b>,
    heap_collection: bool,
}
struct AdmittedSeed<'a, 'b, 'c, S> {
    inner: S,
    admission: AdmissionRef<'a, 'b>,
    sequence: Option<&'c mut SequenceCharge>,
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for AdmittedSeed<'_, '_, '_, S> {
    type Value = S::Value;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        if let Some(sequence) = self.sequence {
            // Only a present element runs this seed. Array exhaustion cannot
            // demand an extra allocation merely to discover the end.
            sequence.element::<S::Value, D::Error>(self.admission)?;
        }
        self.inner.deserialize(Admitted {
            inner: deserializer,
            admission: self.admission,
        })
    }
}

macro_rules! forward_deserialize {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            self.inner.$method(AdmittedVisitor {
                inner: visitor, admission: self.admission, heap_collection: true,
            })
        }
    )*};
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Admitted<'_, '_, D> {
    type Error = D::Error;
    forward_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );
    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_unit_struct(
            name,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_newtype_struct(
            name,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_tuple(
            len,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_tuple_struct(
            name,
            len,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_struct(
            name,
            fields,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_enum(
            name,
            variants,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

macro_rules! forward_visit {
    ($($method:ident($value:ty)),* $(,)?) => {$(
        fn $method<E: serde::de::Error>(self, value: $value) -> Result<Self::Value, E> {
            self.inner.$method(value)
        }
    )*};
}

impl<'de, V: Visitor<'de>> Visitor<'de> for AdmittedVisitor<'_, '_, V> {
    type Value = V::Value;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.expecting(formatter)
    }
    forward_visit!(
        visit_bool(bool),
        visit_i8(i8),
        visit_i16(i16),
        visit_i32(i32),
        visit_i64(i64),
        visit_i128(i128),
        visit_u8(u8),
        visit_u16(u16),
        visit_u32(u32),
        visit_u64(u64),
        visit_u128(u128),
        visit_f32(f32),
        visit_f64(f64),
        visit_char(char),
        visit_str(&str),
        visit_borrowed_str(&'de str),
        visit_string(String),
        visit_bytes(&[u8]),
        visit_borrowed_bytes(&'de [u8]),
        visit_byte_buf(Vec<u8>),
    );
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit()
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_none()
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.inner.visit_some(Admitted {
            inner: deserializer,
            admission: self.admission,
        })
    }
    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        self.inner.visit_newtype_struct(Admitted {
            inner: deserializer,
            admission: self.admission,
        })
    }
    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_seq(AdmittedSequence {
            inner: sequence,
            admission: self.admission,
            charge: self.heap_collection.then(SequenceCharge::default),
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_map(AdmittedMap {
            inner: map,
            admission: self.admission,
            charge: self.heap_collection.then(MapCharge::default),
        })
    }
    fn visit_enum<A: EnumAccess<'de>>(self, value: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_enum(AdmittedEnum {
            inner: value,
            admission: self.admission,
        })
    }
}

struct AdmittedSequence<'a, 'b, A> {
    inner: A,
    admission: AdmissionRef<'a, 'b>,
    charge: Option<SequenceCharge>,
}
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for AdmittedSequence<'_, '_, A> {
    type Error = A::Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        self.inner.next_element_seed(AdmittedSeed {
            inner: seed,
            admission: self.admission,
            sequence: self.charge.as_mut(),
        })
    }
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

struct AdmittedMap<'a, 'b, A> {
    inner: A,
    admission: AdmissionRef<'a, 'b>,
    charge: Option<MapCharge>,
}
impl<'de, A: MapAccess<'de>> MapAccess<'de> for AdmittedMap<'_, '_, A> {
    type Error = A::Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        if let Some(charge) = self.charge.as_mut() {
            charge.key::<K::Value>();
        }
        self.inner.next_key_seed(AdmittedSeed {
            inner: seed,
            admission: self.admission,
            sequence: None,
        })
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        if let Some(charge) = self.charge.as_mut() {
            charge.value::<V::Value, Self::Error>(self.admission)?;
        }
        self.inner.next_value_seed(AdmittedSeed {
            inner: seed,
            admission: self.admission,
            sequence: None,
        })
    }
    fn size_hint(&self) -> Option<usize> {
        None
    }
}

struct AdmittedEnum<'a, 'b, A> {
    inner: A,
    admission: AdmissionRef<'a, 'b>,
}
impl<'de, 'a, 'b, A: EnumAccess<'de>> EnumAccess<'de> for AdmittedEnum<'a, 'b, A> {
    type Error = A::Error;
    type Variant = AdmittedEnum<'a, 'b, A::Variant>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        let (value, variant) = self.inner.variant_seed(AdmittedSeed {
            inner: seed,
            admission: self.admission,
            sequence: None,
        })?;
        Ok((
            value,
            AdmittedEnum {
                inner: variant,
                admission: self.admission,
            },
        ))
    }
}
impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for AdmittedEnum<'_, '_, A> {
    type Error = A::Error;
    fn unit_variant(self) -> Result<(), Self::Error> {
        self.inner.unit_variant()
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        self.inner.newtype_variant_seed(AdmittedSeed {
            inner: seed,
            admission: self.admission,
            sequence: None,
        })
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.tuple_variant(
            len,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.struct_variant(
            fields,
            AdmittedVisitor {
                inner: visitor,
                admission: self.admission,
                heap_collection: false,
            },
        )
    }
}

pub(super) fn decode<'de, T: Deserialize<'de>>(
    json: &'de str,
    initial_required: usize,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(T, usize), CodeIndexProductionErrorV1> {
    let admission = RefCell::new(Admission {
        admit,
        required: initial_required,
        failure: None,
    });
    let mut deserializer = serde_json::Deserializer::from_str(json);
    let decoded = T::deserialize(Admitted {
        inner: &mut deserializer,
        admission: &admission,
    })
    .and_then(|decoded| {
        deserializer.end()?;
        Ok(decoded)
    });
    let admission = admission.into_inner();
    if let Some(failure) = admission.failure {
        return Err(failure);
    }
    let decoded = decoded.map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation manifest payload decoding failed: {error}"
        ))
    })?;
    Ok((decoded, admission.required))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn refusing_vector_growth_stops_before_decoding_its_next_element() {
        static DECODED: AtomicUsize = AtomicUsize::new(0);
        #[derive(Debug)]
        struct Counted([u64; 32]);
        impl<'de> Deserialize<'de> for Counted {
            fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
                let value = u64::deserialize(decoder)?;
                DECODED.fetch_add(1, Ordering::SeqCst);
                Ok(Self([value; 32]))
            }
        }
        DECODED.store(0, Ordering::SeqCst);
        let error = decode::<Vec<Counted>>("[1,2,3,4,5]", 0, &mut |bytes| {
            if bytes > 4 * size_of::<Counted>() {
                return Err(CodeIndexProductionErrorV1::Publication(
                    super::super::super::CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(
                        "fixture collection growth".to_owned(),
                    ),
                ));
            }
            Ok(())
        })
        .expect_err("the second allocation must be refused");
        assert!(matches!(
            error,
            CodeIndexProductionErrorV1::Publication(
                super::super::super::CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
            )
        ));
        assert_eq!(DECODED.load(Ordering::SeqCst), 4);
        let (values, peak) = decode::<Vec<Counted>>("[1,2,3,4,5]", 0, &mut |_| Ok(()))
            .expect("an admitted collection decodes normally");
        assert_eq!(
            values.iter().map(|value| value.0[0]).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert!(peak >= values.capacity() * size_of::<Counted>());
    }
}
