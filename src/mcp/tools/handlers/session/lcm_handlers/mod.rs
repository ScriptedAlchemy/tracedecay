mod expand_query;
mod expansion;
mod lifecycle;
mod retrieval;
mod shared;
mod status;

#[cfg(test)]
mod test_support;

pub(in crate::mcp::tools::handlers) use expand_query::handle_lcm_expand_query;
pub(in crate::mcp::tools::handlers) use expansion::{handle_lcm_describe, handle_lcm_expand};
pub(in crate::mcp::tools::handlers) use lifecycle::{
    handle_lcm_compress, handle_lcm_preflight, handle_lcm_session_boundary,
};
pub(in crate::mcp::tools::handlers) use retrieval::{handle_lcm_grep, handle_lcm_load_session};
pub(in crate::mcp::tools::handlers) use status::{handle_lcm_doctor, handle_lcm_status};

#[cfg(test)]
pub(super) use expand_query::synthesize_expand_query_answer;
