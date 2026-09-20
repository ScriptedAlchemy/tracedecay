//! Memory fact-store tool definitions.

use serde_json::Value;

use super::{def, def_rw};
use crate::{McpCatalogError, ToolDefinition};

struct MemoryTool {
    operation: &'static str,
    title: &'static str,
    description: &'static str,
    write: bool,
}

const MEMORY_TOOLS: &[MemoryTool] = &[
    MemoryTool {
        operation: "fact_store_add",
        title: "Fact Store Add",
        description: "Add one holographic memory fact. The result includes a write-time diff report for near duplicates, possible conflicts, and rejected secret-like content. Calibrate trust to the evidence instead of defaulting high.",
        write: true,
    },
    MemoryTool {
        operation: "fact_store_search",
        title: "Fact Store Search",
        description: "Search durable project or user memory facts by text and trust.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_probe",
        title: "Fact Store Probe",
        description: "Find holographic memory facts connected to one entity.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_related",
        title: "Fact Store Related",
        description: "List entities related to one entity through holographic memory facts.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_reason",
        title: "Fact Store Reason",
        description: "Reason over holographic memory facts connecting multiple entities.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_contradict",
        title: "Fact Store Contradict",
        description: "Find potentially contradictory holographic memory facts above an optional threshold.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_get",
        title: "Fact Store Get",
        description: "Get one holographic memory fact, including trust history explaining score changes.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_update",
        title: "Fact Store Update",
        description: "Update one existing holographic memory fact without changing its identity.",
        write: true,
    },
    MemoryTool {
        operation: "fact_store_remove",
        title: "Fact Store Remove",
        description: "Remove one holographic memory fact by exact fact id.",
        write: true,
    },
    MemoryTool {
        operation: "fact_store_supersede",
        title: "Fact Store Supersede",
        description: "Mark one holographic memory fact as superseded by another fact id. The old fact leaves default list/search/probe results but stays readable by id through its history; payload and trust are untouched. Use this when a newer fact corrects an older one instead of removing the old one.",
        write: true,
    },
    MemoryTool {
        operation: "fact_store_list",
        title: "Fact Store List",
        description: "List holographic memory facts with optional category, trust, and project selectors.",
        write: false,
    },
    MemoryTool {
        operation: "fact_feedback",
        title: "Fact Feedback",
        description: "Record whether an active-project memory fact materially helped or misled the current work, and adjust its trust score.",
        write: true,
    },
    MemoryTool {
        operation: "memory_status",
        title: "Memory Status",
        description: "Inspect canonical memory state: return the owner, fact/entity counts, algebra identity and capacity, trust distribution, below-threshold facts, feedback totals, and retrieval funnel. Defaults to the active project; pass an exact project_selector only when intentionally checking another registered project. Human/operator equivalent: `tracedecay memory status`.",
        write: false,
    },
    MemoryTool {
        operation: "fact_store_curate",
        title: "Fact Store Curate",
        description: "Run the daemon-owned automatic Memory Curator. Callers may bound review size and confidence only; TraceDecay derives the run, operations, validation, policy, and apply authority. Inspect the durable terminal with the read-only automation run tools.",
        write: true,
    },
];

pub(super) fn memory_definitions(
    mut schema: impl FnMut(&str) -> Result<Value, McpCatalogError>,
) -> Result<Vec<ToolDefinition>, McpCatalogError> {
    MEMORY_TOOLS
        .iter()
        .map(|tool| {
            let name = format!("tracedecay_{}", tool.operation);
            let input_schema = schema(tool.operation)?;
            Ok(if tool.write {
                def_rw(&name, tool.title, tool.description, input_schema)
            } else {
                def(&name, tool.title, tool.description, input_schema)
            })
        })
        .collect()
}
