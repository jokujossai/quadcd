//! Changed-unit detection and activation.

mod execute;
mod files;
mod plan;

pub(crate) use execute::{execute_activation, stop_deleted_units_inner};
pub(crate) use files::{all_unit_files, is_template_unit, is_unit_file, unit_name_for_restart};
pub(crate) use plan::{plan_activation, ActivationPlan};
