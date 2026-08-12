//! Dashboard-facing operations over canonical project-memory identities.

use tracedecay_domain::FactId;
use tracedecay_store::{
    FactReadControl, ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactStore, ProjectMemoryMemoryStatusV1,
};

use super::MemoryApplication;
use super::error::MemoryApplicationError;

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    fn fact_identity(
        &self,
        fact_id: FactId,
    ) -> Result<ProjectMemoryFactIdV1, MemoryApplicationError> {
        ProjectMemoryFactIdV1::new(self.owner.clone(), fact_id).map_err(Into::into)
    }

    /// Finite dashboard overview; the dashboard never opens a memory database
    /// or constructs an unbounded store query itself.
    pub async fn dashboard_overview(
        &self,
        fact_limit: usize,
        graph_limit: usize,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryDashboardMemoryOverviewV1, MemoryApplicationError> {
        let overview = self
            .authority
            .dashboard_project_memory_overview(
                ProjectMemoryDashboardMemoryOverviewQueryV1::new(
                    self.owner.clone(),
                    fact_limit,
                    graph_limit,
                )?,
                read_control,
            )
            .await?;
        if overview.owner != self.owner
            || overview.facts.len() > fact_limit
            || overview.entities.len() > graph_limit
            || overview.fact_entity_links.len() > graph_limit
            || overview
                .facts
                .iter()
                .any(|fact| fact.fact.owner() != &self.owner)
            || overview
                .entities
                .iter()
                .any(|entity| entity.target.owner() != &self.owner)
            || overview
                .fact_entity_links
                .iter()
                .any(|link| link.fact.owner() != &self.owner || link.entity.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard overview owner and bounds",
            });
        }
        Ok(overview)
    }

    pub async fn dashboard_fact_detail(
        &self,
        fact_id: FactId,
        read_control: &FactReadControl,
    ) -> Result<Option<ProjectMemoryDashboardFactDetailV1>, MemoryApplicationError> {
        let target = self.fact_identity(fact_id)?;
        let detail = self
            .authority
            .dashboard_project_memory_fact_detail(
                ProjectMemoryDashboardFactDetailQueryV1::new(target.clone())?,
                read_control,
            )
            .await?;
        if let Some(detail) = &detail
            && (detail.fact.owner() != &self.owner
                || detail.fact.fact_id() != target.fact_id()
                || detail
                    .entities
                    .iter()
                    .any(|entity| entity.target.owner() != &self.owner)
                || detail.history.as_ref().is_some_and(|history| {
                    history.owner() != &self.owner || history.fact_id() != target.fact_id()
                }))
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard detail owner and identity",
            });
        }
        Ok(detail)
    }

    pub async fn dashboard_feedback_history(
        &self,
        fact_id: FactId,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.get_project_memory_feedback_history(
            ProjectMemoryFactFeedbackHistoryQueryV1::new(
                self.fact_identity(fact_id)?,
                None,
                limit,
            )?,
            read_control,
        )
        .await
    }

    pub async fn dashboard_memory_status(
        &self,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        self.project_memory_status(read_control).await
    }

    /// Capped vector inputs for dashboard-side PCA and similarity.
    pub async fn dashboard_vector_points(
        &self,
        search: Option<String>,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<Vec<ProjectMemoryDashboardVectorPointV1>, MemoryApplicationError> {
        let points = self
            .authority
            .dashboard_project_memory_vector_points(
                ProjectMemoryDashboardVectorPointsQueryV1::new(self.owner.clone(), search, limit)?,
                read_control,
            )
            .await?;
        if points.len() > limit
            || points
                .iter()
                .any(|point| point.fact.fact.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard vector point owner and bounds",
            });
        }
        Ok(points)
    }

    pub async fn dashboard_oplog(
        &self,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<Vec<ProjectMemoryDashboardOplogEntryV1>, MemoryApplicationError> {
        let entries = self
            .authority
            .dashboard_project_memory_oplog(
                ProjectMemoryDashboardOplogQueryV1::new(self.owner.clone(), limit)?,
                read_control,
            )
            .await?;
        if entries.len() > limit
            || entries.iter().any(|entry| {
                entry
                    .fact
                    .as_ref()
                    .is_some_and(|target| target.owner() != &self.owner)
            })
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard oplog owner and bounds",
            });
        }
        Ok(entries)
    }
}
