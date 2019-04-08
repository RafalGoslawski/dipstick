use std::sync::Arc;
use std::collections::{HashMap};
use std::default::Default;

use core::scheduler::SCHEDULER;
use core::name::{NameParts, MetricName};
use ::{Flush, CancelHandle};
use std::fmt;
use std::time::Duration;
use ::{InputScope, Gauge};
use MetricValue;

/// The actual distribution (random, fixed-cycled, etc) depends on selected sampling method.
#[derive(Debug, Clone, Copy)]
pub enum Sampling {
    /// Record every collected value.
    /// Effectively disable sampling.
    Full,

    /// Floating point sampling rate
    /// - 1.0+ records everything
    /// - 0.5 records one of two values
    /// - 0.0 records nothing
    Random(f64)
}

impl Default for Sampling {
    fn default() -> Sampling {
        Sampling::Full
    }
}

/// A metrics buffering strategy.
/// All strategies other than `Unbuffered` are applied as a best-effort, meaning that the buffer
/// may be flushed at any moment before reaching the limit, for any or no reason in particular.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Buffering {
    /// Do not buffer output.
    Unbuffered,

    /// A buffer of maximum specified size is used.
    BufferSize(usize),

    /// Buffer as much as possible.
    Unlimited,
}

impl Default for Buffering {
    fn default() -> Buffering {
        Buffering::Unbuffered
    }
}

/// Attributes common to metric components.
/// Not all attributes used by all components.
#[derive(Clone, Default)]
pub struct Attributes {
    naming: NameParts,
    sampling: Sampling,
    buffering: Buffering,
    flush_listeners: Vec<Arc<Fn() -> () + Send + Sync + 'static>>,
    tasks: Vec<CancelHandle>,
}

impl fmt::Debug for Attributes {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "naming: {:?}", self.naming)?;
        write!(f, "sampling: {:?}", self.sampling)?;
        write!(f, "buffering: {:?}", self.buffering)
    }
}

/// This trait should not be exposed outside the crate.
pub trait WithAttributes: Clone {
    /// Return attributes of component.
    fn get_attributes(&self) -> &Attributes;

    /// Return attributes of component for mutation.
    // TODO replace with fields-in-traits if ever stabilized (https://github.com/nikomatsakis/fields-in-traits-rfc)
    fn mut_attributes(&mut self) -> &mut Attributes;

    /// Clone the component and mutate its attributes at once.
    fn with_attributes<F: Fn(&mut Attributes)>(&self, edit: F) -> Self {
        let mut cloned = self.clone();
        (edit)(cloned.mut_attributes());
        cloned
    }
}

/// Register and notify scope-flush listeners
pub trait OnFlush {
    /// Notify registered listeners of an impending flush.
    fn notify_flush_listeners(&self);
}

impl <T> OnFlush for T where T: Flush + WithAttributes {
    fn notify_flush_listeners(&self) {
        for listener in &self.get_attributes().flush_listeners {
            (listener)()
        }
    }
}

pub struct ObserveWhen<'a, T, F> {
    target: &'a mut T,
    gauge: Gauge,
    operation: Arc<F>,
}

impl<'a, T, F> ObserveWhen<'a, T, F>
    where F: Fn() -> MetricValue + Send + Sync + 'static,
          T: InputScope + WithAttributes + Send + Sync,
{
    pub fn on_flush(self) {
        let gauge = self.gauge;
        let op = self.operation;
        self.target.mut_attributes().flush_listeners.push(Arc::new(move || gauge.value(op())));
    }

    pub fn every(self, period: Duration,) -> CancelHandle {
        let gauge = self.gauge;
        let op = self.operation;
        let handle = SCHEDULER.schedule(period, move || gauge.value(op()));
        self.target.mut_attributes().tasks.push(handle.clone());
        handle
    }
}

/// Schedule a recurring task
pub trait Observe {

    /// Schedule a recurring task.
    /// The returned handle can be used to cancel the task.
    fn observe<F>(&mut self, gauge: Gauge, operation: F) -> ObserveWhen<Self, F>
        where F: Fn() -> MetricValue + Send + Sync + 'static, Self: Sized;

}

impl<T: InputScope + WithAttributes> Observe for T {
    fn observe<F>(&mut self, gauge: Gauge, operation: F) -> ObserveWhen<Self, F>
        where F: Fn() -> MetricValue + Send + Sync + 'static, Self: Sized
    {
        ObserveWhen {
            target: self,
            gauge,
            operation: Arc::new(operation),
        }
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.cancel()
        }
    }
}

/// Name operations support.
pub trait Prefixed {
    /// Returns namespace of component.
    fn get_prefixes(&self) -> &NameParts;

    /// Append a name to the existing names.
    /// Return a clone of the component with the updated names.
    #[deprecated(since="0.7.2", note="Use named() or add_name()")]
    fn add_prefix<S: Into<String>>(&self, name: S) -> Self;

    /// Append a name to the existing names.
    /// Return a clone of the component with the updated names.
    fn add_name<S: Into<String>>(&self, name: S) -> Self;

    /// Replace any existing names with a single name.
    /// Return a clone of the component with the new name.
    /// If multiple names are required, `add_name` may also be used.
    fn named<S: Into<String>>(&self, name: S) -> Self;

    /// Append any name parts to the name's namespace.
    fn prefix_append<S: Into<MetricName>>(&self, name: S) -> MetricName {
        name.into().append(self.get_prefixes().clone())
    }

    /// Prepend any name parts to the name's namespace.
    fn prefix_prepend<S: Into<MetricName>>(&self, name: S) -> MetricName {
        name.into().prepend(self.get_prefixes().clone())
    }
}

/// Name operations support.
pub trait Label {
    /// Return the namespace of the component.
    fn get_label(&self) -> &Arc<HashMap<String, String>>;

    /// Join namespace and prepend in newly defined metrics.
    fn label(&self, name: &str) -> Self;

}

impl<T: WithAttributes> Prefixed for T {

    /// Returns namespace of component.
    fn get_prefixes(&self) -> &NameParts {
        &self.get_attributes().naming
    }

    /// Append a name to the existing names.
    /// Return a clone of the component with the updated names.
    fn add_prefix<S: Into<String>>(&self, name: S) -> Self {
        self.add_name(name)
    }

    /// Append a name to the existing names.
    /// Return a clone of the component with the updated names.
    fn add_name<S: Into<String>>(&self, name: S) -> Self {
        let name = name.into();
        self.with_attributes(|new_attr| new_attr.naming.push_back(name.clone()))
    }

    /// Replace any existing names with a single name.
    /// Return a clone of the component with the new name.
    /// If multiple names are required, `add_name` may also be used.
    fn named<S: Into<String>>(&self, name: S) -> Self {
        let parts = NameParts::from(name);
        self.with_attributes(|new_attr| new_attr.naming = parts.clone())
    }

}

/// Apply statistical sampling to collected metrics data.
pub trait Sampled: WithAttributes {
    /// Perform random sampling of values according to the specified rate.
    fn sampled(&self, sampling: Sampling) -> Self {
        self.with_attributes(|new_attr| new_attr.sampling = sampling)
    }

    /// Get the sampling strategy for this component, if any.
    fn get_sampling(&self) -> Sampling {
        self.get_attributes().sampling
    }
}

/// Determine scope buffering strategy, if supported by output.
/// Changing this only affects scopes opened afterwards.
/// Buffering is done on best effort, meaning flush will occur if buffer capacity is exceeded.
pub trait Buffered: WithAttributes {
    /// Return a clone with the specified buffering set.
    fn buffered(&self, buffering: Buffering) -> Self {
        self.with_attributes(|new_attr| new_attr.buffering = buffering)
    }

    /// Return the current buffering strategy.
    fn get_buffering(&self) -> Buffering {
        self.get_attributes().buffering
    }

    /// Returns false if the current buffering strategy is `Buffering::Unbuffered`.
    /// Returns true otherwise.
    fn is_buffered(&self) -> bool {
        !(self.get_attributes().buffering == Buffering::Unbuffered)
    }
}

#[cfg(test)]
mod test {
    use output::map::StatsMap;
    use core::attributes::*;
    use core::input::*;
    use core::Flush;
    use core::input::Input;
    use StatsMapScope;

    #[test]
    fn on_flush() {
        let mut metrics: StatsMapScope = StatsMap::default().metrics();
        let gauge = metrics.gauge("my_gauge");
        metrics.observe(gauge, || 4).on_flush();
        metrics.flush().unwrap();
        assert_eq!(Some(&4), metrics.into_map().get("my_gauge"))
    }
}