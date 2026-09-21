//! One row per mounted operation instead of a match per path projection.
//!
//! The catalog id and both route paths are concatenations of the same key and
//! segment, so a second handwritten match can only drift.

macro_rules! mounted_operation_paths {
    (
        with_key,
        id_prefix = $id_prefix:literal,
        route_prefix = $route_prefix:literal,
        $($variant:ident: $key:literal, $segment:literal;)+
    ) => {
        pub const fn operation_key(self) -> &'static str {
            match self {
                $(Self::$variant => $key,)+
            }
        }

        mounted_operation_paths! {
            id_prefix = $id_prefix,
            route_prefix = $route_prefix,
            $($variant: $key, $segment;)+
        }
    };
    (
        id_prefix = $id_prefix:literal,
        route_prefix = $route_prefix:literal,
        $($variant:ident: $key:literal, $segment:literal;)+
    ) => {
        pub const fn operation_id_str(self) -> &'static str {
            match self {
                $(Self::$variant => concat!($id_prefix, $key),)+
            }
        }

        pub const fn route_segment(self) -> &'static str {
            match self {
                $(Self::$variant => $segment,)+
            }
        }

        pub const fn route_path(self) -> &'static str {
            match self {
                $(Self::$variant => concat!("/", $route_prefix, "/", $segment),)+
            }
        }

        pub const fn application_route_path(self) -> &'static str {
            match self {
                $(Self::$variant => concat!("/application/", $route_prefix, "/", $segment),)+
            }
        }
    };
}
