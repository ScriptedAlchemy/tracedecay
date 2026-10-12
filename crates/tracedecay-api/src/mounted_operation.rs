//! One row per mounted operation instead of a match per path projection.
//!
//! The operation enum, its `ALL` list, the catalog id, and both route paths
//! all come from the same `(variant, key, segment)` rows, so a second
//! handwritten list or match can only drift.

macro_rules! mounted_operations {
    (
        $(#[$meta:meta])*
        pub enum $name:ident;
        with_key,
        id_prefix = $id_prefix:literal,
        route_prefix = $route_prefix:literal,
        $($variant:ident: $key:literal, $segment:literal;)+
    ) => {
        mounted_operations! {
            $(#[$meta])*
            pub enum $name;
            id_prefix = $id_prefix,
            route_prefix = $route_prefix,
            $($variant: $key, $segment;)+
        }

        impl $name {
            /// The catalog operation key, as it appears in the operation id.
            pub const fn operation_key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }
        }
    };
    (
        $(#[$meta:meta])*
        pub enum $name:ident;
        id_prefix = $id_prefix:literal,
        route_prefix = $route_prefix:literal,
        $($variant:ident: $key:literal, $segment:literal;)+
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            /// Every mounted operation, in mounted order.
            pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant,)+];

            /// The catalog operation id, as a literal the route documents can hold.
            pub const fn operation_id_str(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!($id_prefix, $key),)+
                }
            }

            /// The final path segment that names this operation on its router.
            pub const fn route_segment(self) -> &'static str {
                match self {
                    $(Self::$variant => $segment,)+
                }
            }

            /// The path this operation answers on the application router.
            pub const fn route_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("/", $route_prefix, "/", $segment),)+
                }
            }

            /// The path the catalog advertises, which the executable nests
            /// under its `/application` prefix.
            pub const fn application_route_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("/application/", $route_prefix, "/", $segment),)+
                }
            }
        }
    };
}
