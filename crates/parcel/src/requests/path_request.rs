use std::hash::Hash;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use parcel_core::plugin::BuildProgressEvent;
use parcel_core::plugin::ReporterEvent;
use parcel_core::plugin::Resolution;
use parcel_core::plugin::ResolveContext;
use parcel_core::plugin::ResolvedResolution;
use parcel_core::plugin::ResolverPlugin;
use parcel_core::plugin::ResolvingEvent;
use parcel_core::types::Dependency;
use parcel_resolver::parse_scheme;

use crate::request_tracker::Request;
use crate::request_tracker::RequestResult;
use crate::request_tracker::RunRequestContext;
use crate::request_tracker::RunRequestError;

#[derive(Hash)]
pub struct PathRequest {
  pub dependency: Arc<Dependency>,
  pub named_pipelines: Vec<String>,
  pub resolvers: Arc<Vec<Box<dyn ResolverPlugin>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathResolution {
  Excluded,
  Resolved {
    can_defer: bool,
    path: PathBuf,
    code: Option<String>,
    pipeline: Option<String>,
    query: Option<String>,
    side_effects: bool,
  },
}

// TODO tracing, dev deps
impl Request<PathResolution> for PathRequest {
  fn id(&self) -> u64 {
    let mut hasher = parcel_core::hash::IdentifierHasher::default();

    self.dependency.hash(&mut hasher);
    self.named_pipelines.hash(&mut hasher);
    self.resolvers.hash(&mut hasher);

    hasher.finish()
  }

  fn run(
    &self,
    request_context: RunRequestContext<PathResolution>,
  ) -> Result<RequestResult<PathResolution>, RunRequestError> {
    request_context.report(ReporterEvent::BuildProgress(BuildProgressEvent::Resolving(
      ResolvingEvent {
        dependency: Arc::clone(&self.dependency),
      },
    )));

    let (parsed_pipeline, specifier) = parse_scheme(&self.dependency.specifier)
      .and_then(|(pipeline, specifier)| {
        if self
          .named_pipelines
          .contains(&String::from(pipeline.as_ref()))
        {
          Ok((Some(pipeline.to_string()), specifier))
        } else {
          Err(())
        }
      })
      .unwrap_or((None, self.dependency.specifier.as_ref()));

    let mut invalidations = Vec::new();

    for resolver in self.resolvers.iter() {
      let resolved = resolver.resolve(ResolveContext {
        dependency: Arc::clone(&self.dependency),
        pipeline: parsed_pipeline.clone(),
        specifier: String::from(specifier),
      })?;

      invalidations.extend(resolved.invalidations);

      match resolved.resolution {
        Resolution::Unresolved => continue,
        Resolution::Excluded => {
          return Ok(RequestResult {
            invalidations: Vec::new(),
            result: PathResolution::Excluded,
          })
        }
        Resolution::Resolved(ResolvedResolution {
          can_defer,
          code,
          file_path,
          meta: _meta,
          pipeline,
          priority: _priority,
          query,
          side_effects,
        }) => {
          if !file_path.is_absolute() {
            return Err(anyhow!(
              "{:?} must return an absolute path, but got {}",
              resolver,
              file_path.display()
            ));
          }

          // TODO resolution.diagnostics
          // TODO Set dependency meta and priority

          return Ok(RequestResult {
            invalidations,
            result: PathResolution::Resolved {
              can_defer,
              code,
              path: file_path,
              pipeline: pipeline
                .or(parsed_pipeline)
                .or(self.dependency.pipeline.clone()),
              query,
              side_effects,
            },
          });
        }
      };
    }

    if self.dependency.is_optional {
      return Ok(RequestResult {
        invalidations,
        result: PathResolution::Excluded,
      });
    }

    let resolve_from = self
      .dependency
      .resolve_from
      .as_ref()
      .or(self.dependency.source_path.as_ref());

    match resolve_from {
      None => Err(anyhow!("Failed to resolve {}", self.dependency.specifier)),
      Some(from) => Err(anyhow!(
        "Failed to resolve {} from {}",
        self.dependency.specifier,
        from.display()
      )),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::fmt::Debug;

  use parcel_core::plugin::Resolved;
  use parcel_core::plugin::ResolvedResolution;

  use crate::request_tracker::RequestTracker;

  use super::*;

  #[derive(Debug, Hash)]
  struct ExcludedResolverPlugin {}

  impl ResolverPlugin for ExcludedResolverPlugin {
    fn resolve(&self, _ctx: ResolveContext) -> Result<Resolved, anyhow::Error> {
      Ok(Resolved {
        invalidations: Vec::new(),
        resolution: Resolution::Excluded,
      })
    }
  }

  struct ResolvedResolverPlugin {
    resolution: ResolvedResolution,
  }

  impl Debug for ResolvedResolverPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "ResolvedResolverPlugin")
    }
  }

  impl Hash for ResolvedResolverPlugin {
    fn hash<H: Hasher>(&self, _state: &mut H) {}
  }

  impl ResolverPlugin for ResolvedResolverPlugin {
    fn resolve(&self, _ctx: ResolveContext) -> Result<Resolved, anyhow::Error> {
      Ok(Resolved {
        invalidations: Vec::new(),
        resolution: Resolution::Resolved(self.resolution.clone()),
      })
    }
  }

  #[derive(Debug, Hash)]
  struct UnresolvedResolverPlugin {}

  impl ResolverPlugin for UnresolvedResolverPlugin {
    fn resolve(&self, _ctx: ResolveContext) -> Result<Resolved, anyhow::Error> {
      Ok(Resolved {
        invalidations: Vec::new(),
        resolution: Resolution::Unresolved,
      })
    }
  }

  #[test]
  fn returns_excluded_resolution() {
    let request = PathRequest {
      dependency: Arc::new(Dependency::default()),
      named_pipelines: Vec::new(),
      resolvers: Arc::new(vec![Box::new(ExcludedResolverPlugin {})]),
    };

    let resolution = request.run(RunRequestContext::new(None, &mut RequestTracker::new()));

    assert_eq!(
      resolution.map_err(|e| e.to_string()),
      Ok(RequestResult {
        result: PathResolution::Excluded,
        invalidations: Vec::new(),
      })
    );
  }

  #[test]
  fn returns_an_error_when_resolved_file_path_is_not_absolute() {
    let request = PathRequest {
      dependency: Arc::new(Dependency::default()),
      named_pipelines: Vec::new(),
      resolvers: Arc::new(vec![Box::new(ResolvedResolverPlugin {
        resolution: ResolvedResolution {
          file_path: PathBuf::from("./"),
          ..ResolvedResolution::default()
        },
      })]),
    };

    let resolution = request.run(RunRequestContext::new(None, &mut RequestTracker::new()));

    assert_eq!(
      resolution.map_err(|e| e.to_string()),
      Err(String::from(
        "ResolvedResolverPlugin must return an absolute path, but got ./"
      ))
    );
  }

  #[test]
  fn returns_the_first_resolved_resolution() {
    #[cfg(not(target_os = "windows"))]
    let root = PathBuf::from(std::path::MAIN_SEPARATOR_STR);

    #[cfg(target_os = "windows")]
    let root = PathBuf::from("c:\\windows");

    let request = PathRequest {
      dependency: Arc::new(Dependency::default()),
      named_pipelines: Vec::new(),
      resolvers: Arc::new(vec![
        Box::new(UnresolvedResolverPlugin {}),
        Box::new(ResolvedResolverPlugin {
          resolution: ResolvedResolution {
            file_path: root.join("a.js"),
            ..ResolvedResolution::default()
          },
        }),
        Box::new(ResolvedResolverPlugin {
          resolution: ResolvedResolution {
            file_path: root.join("b.js"),
            ..ResolvedResolution::default()
          },
        }),
      ]),
    };

    let resolution = request.run(RunRequestContext::new(None, &mut RequestTracker::new()));

    assert_eq!(
      resolution.map_err(|e| e.to_string()),
      Ok(RequestResult {
        result: PathResolution::Resolved {
          can_defer: false,
          code: None,
          path: root.join("a.js"),
          pipeline: None,
          query: None,
          side_effects: false
        },
        invalidations: Vec::new(),
      })
    );
  }

  mod when_all_resolvers_return_unresolved {
    use super::*;

    #[test]
    fn returns_an_excluded_resolution_when_the_dependency_is_optional() {
      let request = PathRequest {
        dependency: Arc::new(Dependency {
          is_optional: true,
          specifier: String::from("a.js"),
          ..Default::default()
        }),
        named_pipelines: Vec::new(),
        resolvers: Arc::new(vec![Box::new(UnresolvedResolverPlugin {})]),
      };

      let resolution = request.run(RunRequestContext::new(None, &mut RequestTracker::new()));

      assert_eq!(
        resolution.map_err(|e| e.to_string()),
        Ok(RequestResult {
          result: PathResolution::Excluded,
          invalidations: Vec::new(),
        })
      );
    }

    #[test]
    fn returns_an_error_when_the_dependency_is_required() {
      let assert_error = |dependency: Dependency, error: &str| {
        let request = PathRequest {
          dependency: Arc::new(Dependency {
            is_optional: false,
            ..dependency
          }),
          named_pipelines: Vec::new(),
          resolvers: Arc::new(vec![Box::new(UnresolvedResolverPlugin {})]),
        };

        let resolution = request.run(RunRequestContext::new(None, &mut RequestTracker::new()));

        assert_eq!(
          resolution.map_err(|e| e.to_string()),
          Err(String::from(error))
        );
      };

      assert_error(
        Dependency {
          specifier: String::from("a.js"),
          ..Dependency::default()
        },
        "Failed to resolve a.js",
      );

      assert_error(
        Dependency {
          resolve_from: Some(PathBuf::from("rf.js")),
          specifier: String::from("a.js"),
          ..Dependency::default()
        },
        "Failed to resolve a.js from rf.js",
      );

      assert_error(
        Dependency {
          source_path: Some(PathBuf::from("sp.js")),
          specifier: String::from("a.js"),
          ..Dependency::default()
        },
        "Failed to resolve a.js from sp.js",
      );

      assert_error(
        Dependency {
          resolve_from: Some(PathBuf::from("rf.js")),
          source_path: Some(PathBuf::from("sp.js")),
          specifier: String::from("a.js"),
          ..Dependency::default()
        },
        "Failed to resolve a.js from rf.js",
      );
    }
  }
}
