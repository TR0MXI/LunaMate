//! Cubism expression playback and blending.
//!
//! Expressions are short parameter blends loaded from `exp3.json` files. Use
//! [`ExpressionPlayer`] for one expression, or [`ExpressionManager`] when the app
//! should fade older expressions out as newer ones start.

use std::{collections::HashSet, fs, path::Path};

use crate::{
    json::{
        Expression3, ExpressionBlend, ExpressionParameter, apply_expression_parameter, easing_sine,
    },
    runtime::ModelRuntime,
};

const MAX_ACTIVE_EXPRESSION_PLAYERS: usize = 8;

#[derive(Debug, Clone)]
/// Plays one parsed `exp3.json` expression against a [`ModelRuntime`].
pub struct ExpressionPlayer {
    expression: Expression3,
    time: f32,
    weight: f32,
    fade_out_started_at: Option<f32>,
    finished: bool,
}

impl ExpressionPlayer {
    /// Creates a player at time `0.0` with full weight.
    pub fn new(expression: Expression3) -> Self {
        Self {
            expression,
            time: 0.0,
            weight: 1.0,
            fade_out_started_at: None,
            finished: false,
        }
    }

    /// Returns the expression data owned by this player.
    pub fn expression(&self) -> &Expression3 {
        &self.expression
    }

    /// Returns the current playback time in seconds.
    pub fn time(&self) -> f32 {
        self.time
    }

    /// Returns the player's global blend weight.
    pub fn weight(&self) -> f32 {
        self.weight
    }

    /// Sets the player's global blend weight, clamped to `0.0..=1.0`.
    pub fn set_weight(&mut self, weight: f32) {
        self.weight = weight.clamp(0.0, 1.0);
    }

    /// Returns the combined global, fade-in, and fade-out weight.
    pub fn fade_weight(&self) -> f32 {
        self.weight * self.fade_in_weight() * self.fade_out_weight()
    }

    /// Returns whether this expression is currently fading out.
    pub fn is_fading_out(&self) -> bool {
        self.fade_out_started_at.is_some()
    }

    /// Returns whether this expression has fully faded out.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Returns whether the player still changes its blend weight over time.
    pub fn needs_tick(&self) -> bool {
        if self.finished {
            return false;
        }
        self.fade_out_started_at.is_some() || self.time < self.expression.resolved_fade_in_time()
    }

    /// Restarts the expression and cancels any active fade-out.
    pub fn restart(&mut self) {
        self.time = 0.0;
        self.fade_out_started_at = None;
        self.finished = false;
    }

    /// Begins fading the expression out.
    ///
    /// If the expression declares a zero fade-out time, it finishes immediately.
    pub fn start_fade_out(&mut self) {
        if self.finished || self.fade_out_started_at.is_some() {
            return;
        }

        let fade_out = self.expression.resolved_fade_out_time();
        if fade_out == 0.0 {
            self.finished = true;
        } else {
            self.fade_out_started_at = Some(self.time);
        }
    }

    /// Advances expression time by `delta_seconds`.
    ///
    /// Negative deltas are treated as zero.
    pub fn tick(&mut self, delta_seconds: f32) {
        if self.finished {
            return;
        }

        self.time += delta_seconds.max(0.0);
        if let Some(fade_out_started_at) = self.fade_out_started_at {
            let fade_out = self.expression.resolved_fade_out_time();
            if self.time - fade_out_started_at >= fade_out {
                self.finished = true;
            }
        }
    }

    /// Applies this expression's current parameter values to a runtime.
    ///
    /// Unknown parameter ids are ignored. Call [`ModelRuntime::update_meshes`]
    /// after all parameter-producing systems have run for the frame.
    pub fn apply(&self, runtime: &mut ModelRuntime) {
        if self.finished {
            return;
        }

        let weight = self.fade_weight();
        for parameter in self.expression.parameters() {
            let Some(index) = runtime.parameter_index(parameter.id()) else {
                continue;
            };
            let Some(current) = runtime.parameter_value_by_index(index) else {
                continue;
            };
            let value = apply_expression_parameter(current, parameter, weight);
            runtime.set_parameter_by_index(index, value);
        }
    }

    fn fade_in_weight(&self) -> f32 {
        let fade_in = self.expression.resolved_fade_in_time();
        if fade_in == 0.0 {
            1.0
        } else {
            easing_sine(self.time / fade_in)
        }
    }

    fn expression_weight(&self) -> f32 {
        self.weight * self.fade_in_weight()
    }

    fn fade_out_weight(&self) -> f32 {
        let Some(fade_out_started_at) = self.fade_out_started_at else {
            return 1.0;
        };
        let fade_out = self.expression.resolved_fade_out_time();
        if fade_out == 0.0 {
            0.0
        } else {
            easing_sine((fade_out - (self.time - fade_out_started_at)) / fade_out)
        }
    }
}

#[derive(Debug, Clone, Default)]
/// Manages a stack of expressions with Cubism-style fade transitions.
///
/// Calling [`play`](Self::play) starts a new expression and fades out older
/// players. The manager keeps overlapping players long enough to blend smoothly,
/// then drops finished expressions during [`tick`](Self::tick).
pub struct ExpressionManager {
    players: Vec<ExpressionPlayer>,
    parameter_ids: Vec<String>,
}

impl ExpressionManager {
    /// Creates an empty expression manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts an expression and fades out currently active expressions.
    pub fn play(&mut self, expression: Expression3) {
        for player in &mut self.players {
            player.start_fade_out();
        }
        let overflow = self
            .players
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_ACTIVE_EXPRESSION_PLAYERS);
        if overflow > 0 {
            self.players.drain(..overflow);
        }
        self.players.push(ExpressionPlayer::new(expression));
        self.rebuild_parameter_ids();
    }

    /// Starts fading out every active expression.
    pub fn stop_all(&mut self) {
        for player in &mut self.players {
            player.start_fade_out();
        }
    }

    /// Advances all active expression players by `delta_seconds`.
    pub fn tick(&mut self, delta_seconds: f32) {
        let previous_count = self.players.len();
        for player in &mut self.players {
            player.tick(delta_seconds);
        }
        self.players.retain(|player| !player.is_finished());
        if self.players.len() != previous_count {
            self.rebuild_parameter_ids();
        }
    }

    /// Applies all active expressions to a runtime.
    ///
    /// The manager combines additive, multiply, and overwrite blends before
    /// writing parameter values.
    pub fn apply(&self, runtime: &mut ModelRuntime) {
        let weight = combined_expression_weight(&self.players);
        if weight == 0.0 {
            return;
        }

        for id in &self.parameter_ids {
            let Some(index) = runtime.parameter_index(id) else {
                continue;
            };
            let Some(base) = runtime.parameter_value_by_index(index) else {
                continue;
            };
            let mut values = ExpressionBlendValues::identity(base);
            for (active_index, player) in self
                .players
                .iter()
                .filter(|player| !player.is_finished())
                .enumerate()
            {
                let operation = player
                    .expression()
                    .parameters()
                    .iter()
                    .find(|candidate| candidate.id() == id);
                let next = ExpressionBlendValues::from_parameter(base, operation);
                if active_index == 0 {
                    values = next;
                } else {
                    values.blend_towards(next, player.fade_weight());
                }
            }
            let target = values.resolved();
            let value = base * (1.0 - weight) + target * weight;
            runtime.set_parameter_by_index(index, value);
        }
    }

    /// Returns the number of expression players still being blended.
    pub fn active_expression_count(&self) -> usize {
        self.players.len()
    }

    /// Returns whether there are no active expression players.
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// Returns whether any player still has an active fade transition.
    pub fn needs_tick(&self) -> bool {
        self.players.iter().any(ExpressionPlayer::needs_tick)
    }

    fn rebuild_parameter_ids(&mut self) {
        self.parameter_ids.clear();
        let mut seen = HashSet::new();
        for player in &self.players {
            if player.is_finished() {
                continue;
            }
            for parameter in player.expression().parameters() {
                if seen.insert(parameter.id()) {
                    self.parameter_ids.push(parameter.id().to_owned());
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ExpressionBlendValues {
    additive: f32,
    multiply: f32,
    overwrite: f32,
}

impl ExpressionBlendValues {
    fn identity(base: f32) -> Self {
        Self {
            additive: 0.0,
            multiply: 1.0,
            overwrite: base,
        }
    }

    fn from_parameter(base: f32, parameter: Option<&ExpressionParameter>) -> Self {
        let Some(parameter) = parameter else {
            return Self::identity(base);
        };
        match parameter.blend() {
            ExpressionBlend::Add => Self {
                additive: parameter.value(),
                multiply: 1.0,
                overwrite: base,
            },
            ExpressionBlend::Multiply => Self {
                additive: 0.0,
                multiply: parameter.value(),
                overwrite: base,
            },
            ExpressionBlend::Overwrite => Self {
                additive: 0.0,
                multiply: 1.0,
                overwrite: parameter.value(),
            },
        }
    }

    fn blend_towards(&mut self, next: Self, weight: f32) {
        self.additive = lerp(self.additive, next.additive, weight);
        self.multiply = lerp(self.multiply, next.multiply, weight);
        self.overwrite = lerp(self.overwrite, next.overwrite, weight);
    }

    fn resolved(self) -> f32 {
        (self.overwrite + self.additive) * self.multiply
    }
}

fn lerp(from: f32, to: f32, weight: f32) -> f32 {
    from * (1.0 - weight) + to * weight
}

fn combined_expression_weight(players: &[ExpressionPlayer]) -> f32 {
    let active = players.iter().filter(|player| !player.is_finished());
    let all_fading_out = active.clone().all(ExpressionPlayer::is_fading_out);
    let weight = if all_fading_out {
        active
            .map(ExpressionPlayer::fade_weight)
            .fold(0.0, f32::max)
    } else {
        players
            .iter()
            .filter(|player| !player.is_finished())
            .map(ExpressionPlayer::expression_weight)
            .sum()
    };
    weight.clamp(0.0, 1.0)
}

/// Loads a Cubism `exp3.json` file from disk.
pub fn load_expression(path: impl AsRef<Path>) -> Result<Expression3, ExpressionLoadError> {
    let path = path.as_ref();
    let source = fs::read_to_string(path).map_err(|source| ExpressionLoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Expression3::from_json_str(&source).map_err(ExpressionLoadError::Parse)
}

#[derive(Debug, thiserror::Error)]
/// Errors that can occur while loading an expression file.
pub enum ExpressionLoadError {
    /// The expression file could not be read.
    #[error("failed to read {path}: {source}")]
    Io {
        /// Path of the file that failed to load.
        path: String,
        /// Original I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The expression JSON was invalid or unsupported.
    #[error("failed to parse exp3: {0}")]
    Parse(#[source] crate::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overwrite_expression(value: f32) -> Expression3 {
        Expression3::from_json_str(&format!(
            r#"{{"Type":"Live2D Expression","Parameters":[{{"Id":"ParamAngleX","Value":{value},"Blend":"Overwrite"}}]}}"#
        ))
        .expect("test expression must parse")
    }

    #[test]
    fn overlapping_overwrites_are_aggregated_before_application() {
        let old = overwrite_expression(1.0);
        let new = overwrite_expression(0.0);
        let mut values = ExpressionBlendValues::from_parameter(0.0, old.parameters().first());

        values.blend_towards(
            ExpressionBlendValues::from_parameter(0.0, new.parameters().first()),
            0.5,
        );

        assert!((values.resolved() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn replacement_keeps_full_expression_weight_while_old_player_fades_out() {
        let expression = overwrite_expression(1.0);
        let mut old = ExpressionPlayer::new(expression.clone());
        old.tick(1.0);
        old.start_fade_out();
        let mut new = ExpressionPlayer::new(expression);
        new.tick(0.5);

        assert!((combined_expression_weight(&[old, new]) - 1.0).abs() < f32::EPSILON);
    }
}
