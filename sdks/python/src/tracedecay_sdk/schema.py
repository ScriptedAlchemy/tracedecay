"""Runtime validation for canonical generated JSON Schema contracts."""

from __future__ import annotations

import json
from typing import TypeAlias, cast

JsonScalar: TypeAlias = str | int | float | bool | None
JsonValue: TypeAlias = JsonScalar | list["JsonValue"] | dict[str, "JsonValue"]
JsonSchema: TypeAlias = dict[str, JsonValue]


def decode_canonical_schema(value: object, schema: JsonSchema) -> JsonValue:
    """Validate one JSON value against a generated canonical schema."""

    _validate(value, schema, schema, "$")
    return cast(JsonValue, value)


def _validate(value: object, schema: JsonSchema, root: JsonSchema, path: str) -> None:
    reference = schema.get("$ref")
    if isinstance(reference, str):
        prefix = "#/$defs/"
        if not reference.startswith(prefix):
            raise TypeError(f"{path}: unsupported schema reference")
        definitions = root.get("$defs")
        if not isinstance(definitions, dict):
            raise TypeError(f"{path}: schema definitions are missing")
        definition_map = cast(dict[str, object], definitions)
        target = definition_map.get(reference[len(prefix) :])
        if not isinstance(target, dict):
            raise TypeError(f"{path}: schema reference is missing")
        _validate(value, cast(JsonSchema, target), root, path)
        return

    if "const" in schema and value != schema["const"]:
        raise TypeError(f"{path}: value does not match the canonical constant")
    enum = schema.get("enum")
    if isinstance(enum, list) and value not in cast(list[object], enum):
        raise TypeError(f"{path}: value is not a canonical enum member")

    for keyword, exact in (("anyOf", False), ("oneOf", True)):
        variants = schema.get(keyword)
        if isinstance(variants, list):
            matches = 0
            for variant in cast(list[object], variants):
                if not isinstance(variant, dict):
                    continue
                try:
                    _validate(value, cast(JsonSchema, variant), root, path)
                    matches += 1
                except TypeError:
                    pass
            if matches == 0 or (exact and matches != 1):
                raise TypeError(f"{path}: value does not match the canonical union")
            return

    schema_type = schema.get("type")
    if schema_type == "null":
        if value is not None:
            raise TypeError(f"{path}: expected null")
    elif schema_type == "boolean":
        if not isinstance(value, bool):
            raise TypeError(f"{path}: expected boolean")
    elif schema_type == "integer":
        if not isinstance(value, int) or isinstance(value, bool):
            raise TypeError(f"{path}: expected integer")
        integer_format = schema.get("format")
        bounds = {
            "uint32": (0, 2**32 - 1),
            "uint64": (0, 2**64 - 1),
            "int64": (-(2**63), 2**63 - 1),
        }.get(integer_format if isinstance(integer_format, str) else "")
        if bounds is not None and not bounds[0] <= value <= bounds[1]:
            raise TypeError(f"{path}: integer is outside canonical {integer_format}")
        _validate_number(value, schema, path)
    elif schema_type == "number":
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            raise TypeError(f"{path}: expected number")
        _validate_number(value, schema, path)
    elif schema_type == "string":
        if not isinstance(value, str):
            raise TypeError(f"{path}: expected string")
    elif schema_type == "array":
        if not isinstance(value, list):
            raise TypeError(f"{path}: expected array")
        items = cast(list[object], value)
        item_schema = schema.get("items")
        if not isinstance(item_schema, dict):
            raise TypeError(f"{path}: canonical array schema has no items")
        for index, item in enumerate(items):
            _validate(item, cast(JsonSchema, item_schema), root, f"{path}[{index}]")
        if schema.get("uniqueItems") is True:
            encoded = [json.dumps(item, sort_keys=True) for item in items]
            if len(encoded) != len(set(encoded)):
                raise TypeError(f"{path}: array items must be unique")
    elif schema_type == "object":
        if not isinstance(value, dict):
            raise TypeError(f"{path}: expected object")
        raw_object = cast(dict[object, object], value)
        if any(not isinstance(key, str) for key in raw_object):
            raise TypeError(f"{path}: expected object")
        item_object = cast(dict[str, object], raw_object)
        properties = schema.get("properties")
        if not isinstance(properties, dict):
            raise TypeError(f"{path}: canonical object schema has no properties")
        property_map = cast(dict[str, object], properties)
        required = schema.get("required")
        if isinstance(required, list):
            for field in cast(list[object], required):
                if isinstance(field, str) and field not in item_object:
                    raise TypeError(f"{path}.{field}: required field is missing")
        if schema.get("additionalProperties") is False:
            unexpected = set(item_object) - set(property_map)
            if unexpected:
                raise TypeError(f"{path}: unexpected field {min(unexpected)}")
        for field, item in item_object.items():
            field_schema = property_map.get(field)
            if isinstance(field_schema, dict):
                _validate(
                    item,
                    cast(JsonSchema, field_schema),
                    root,
                    f"{path}.{field}",
                )
    elif schema_type is not None:
        raise TypeError(f"{path}: unsupported canonical schema type")


def _validate_number(
    value: int | float, schema: JsonSchema, path: str
) -> None:
    minimum = schema.get("minimum")
    if isinstance(minimum, (int, float)) and value < minimum:
        raise TypeError(f"{path}: number is below the canonical minimum")
    maximum = schema.get("maximum")
    if isinstance(maximum, (int, float)) and value > maximum:
        raise TypeError(f"{path}: number is above the canonical maximum")
