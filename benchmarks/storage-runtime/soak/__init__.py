"""Pure-stdlib planning and evidence gates for storage-runtime soak campaigns.

This package deliberately does not discover profiles, launch TraceDecay, or migrate
stores.  Callers must provide baseline result artifact paths and observations from an
external, isolated fixture runner.
"""

from .evidence import BaselineSet, assess_evidence, load_baselines
from .scheduler import CampaignConfig, build_campaign
from .trends import RESOURCE_NAMES, TrendPolicy, evaluate_resource_trends

__all__ = [
    "BaselineSet",
    "CampaignConfig",
    "RESOURCE_NAMES",
    "TrendPolicy",
    "assess_evidence",
    "build_campaign",
    "evaluate_resource_trends",
    "load_baselines",
]
