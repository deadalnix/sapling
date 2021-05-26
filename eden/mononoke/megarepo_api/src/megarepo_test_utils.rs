/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{anyhow, Error};
use blobrepo::BlobRepo;
use commit_transformation::create_source_to_target_multi_mover;
use context::CoreContext;
use maplit::btreemap;
use megarepo_config::{
    MononokeMegarepoConfigs, SourceRevision, SyncConfigVersion, Target, TestMononokeMegarepoConfigs,
};
use megarepo_mapping::{
    CommitRemappingState, MegarepoMapping, Source, SourceMappingRules, SyncTargetConfig,
};
use mononoke_api::Mononoke;
use mononoke_types::{ChangesetId, RepositoryId};
use std::{collections::BTreeMap, sync::Arc};
use test_repo_factory::TestRepoFactory;
use tests_utils::{bookmark, list_working_copy_utf8, resolve_cs_id, CreateCommitContext};

pub struct MegarepoTest {
    pub blobrepo: BlobRepo,
    pub megarepo_mapping: Arc<MegarepoMapping>,
    pub mononoke: Arc<Mononoke>,
    pub configs_storage: TestMononokeMegarepoConfigs,
}

impl MegarepoTest {
    pub async fn new(ctx: &CoreContext) -> Result<Self, Error> {
        let id = RepositoryId::new(0);
        let mut factory = TestRepoFactory::new()?;
        let megarepo_mapping = factory.megarepo_mapping();
        let blobrepo: BlobRepo = factory.with_id(id).build()?;
        let mononoke = Arc::new(
            Mononoke::new_test(ctx.clone(), vec![("repo".to_string(), blobrepo.clone())]).await?,
        );
        let configs_storage = TestMononokeMegarepoConfigs::new(ctx.logger());

        Ok(Self {
            blobrepo,
            megarepo_mapping,
            mononoke,
            configs_storage,
        })
    }

    pub fn repo_id(&self) -> RepositoryId {
        self.blobrepo.get_repoid()
    }

    pub fn target(&self, bookmark: String) -> Target {
        Target {
            repo_id: self.repo_id().id() as i64,
            bookmark,
        }
    }

    pub async fn prepare_initial_commit_in_target(
        &self,
        ctx: &CoreContext,
        version: &SyncConfigVersion,
        target: &Target,
    ) -> Result<(), Error> {
        let initial_config = self.configs_storage.get_config_by_version(
            ctx.clone(),
            target.clone(),
            version.clone(),
        )?;

        let mut init_target_cs = CreateCommitContext::new_root(&ctx, &self.blobrepo);

        let mut remapping_state = btreemap! {};
        for source in initial_config.sources {
            let mover = create_source_to_target_multi_mover(source.mapping.clone())?;
            let init_source_cs_id = match source.revision {
                SourceRevision::bookmark(bookmark) => {
                    resolve_cs_id(&ctx, &self.blobrepo, bookmark).await?
                }
                SourceRevision::hash(hash) => {
                    let cs_id = ChangesetId::from_bytes(hash)?;
                    resolve_cs_id(&ctx, &self.blobrepo, cs_id).await?
                }
                _ => {
                    unimplemented!()
                }
            };
            let source_wc = list_working_copy_utf8(&ctx, &self.blobrepo, init_source_cs_id).await?;

            for (file, content) in source_wc {
                let target_files = mover(&file)?;
                for target_file in target_files {
                    init_target_cs = init_target_cs.add_file(target_file, content.clone());
                }
            }
            remapping_state.insert(source.source_name, init_source_cs_id);
        }

        let mut init_target_cs = init_target_cs.create_commit_object().await?;
        let remapping_state =
            CommitRemappingState::new(remapping_state, initial_config.version.clone());
        remapping_state
            .save_in_changeset(ctx, &self.blobrepo, &mut init_target_cs)
            .await?;
        let init_target_cs = init_target_cs.freeze()?;
        let init_target_cs_id = init_target_cs.get_changeset_id();
        blobrepo::save_bonsai_changesets(vec![init_target_cs], ctx.clone(), self.blobrepo.clone())
            .await?;

        bookmark(&ctx, &self.blobrepo, target.bookmark.clone())
            .set_to(init_target_cs_id)
            .await?;
        Ok(())
    }
}

pub struct SyncTargetConfigBuilder {
    repo_id: RepositoryId,
    target: Target,
    version: SyncConfigVersion,
    sources: Vec<Source>,
}

impl SyncTargetConfigBuilder {
    pub fn new(repo_id: RepositoryId, target: Target, version: SyncConfigVersion) -> Self {
        Self {
            repo_id,
            target,
            version,
            sources: vec![],
        }
    }

    pub fn source_builder(self, source_name: String) -> SourceVersionBuilder {
        SourceVersionBuilder::new(source_name, self.repo_id, self)
    }

    pub fn add_source(&mut self, source: Source) {
        self.sources.push(source)
    }

    pub fn build(self, configs_storage: &mut TestMononokeMegarepoConfigs) {
        let config = SyncTargetConfig {
            target: self.target.clone(),
            sources: self.sources,
            version: self.version.clone(),
        };

        configs_storage.add((self.target, self.version), config);
    }
}

pub struct SourceVersionBuilder {
    source_name: String,
    git_repo_name: String,
    default_prefix: Option<String>,
    source_bookmark: Option<String>,
    repo_id: RepositoryId,
    config_builder: SyncTargetConfigBuilder,
}

impl SourceVersionBuilder {
    pub fn new(
        source_name: String,
        repo_id: RepositoryId,
        config_builder: SyncTargetConfigBuilder,
    ) -> Self {
        Self {
            source_name: source_name.clone(),
            // This field won't be used much in tests, so just set to the same
            // value as source_name
            git_repo_name: source_name,
            default_prefix: None,
            source_bookmark: None,
            repo_id,
            config_builder,
        }
    }

    pub fn set_prefix_bookmark_to_source_name(mut self) -> Self {
        self.default_prefix = Some(self.source_name.clone());
        self.source_bookmark = Some(self.source_name.clone());
        self
    }

    #[allow(unused)]
    pub fn default_prefix(mut self, default_prefix: String) -> Self {
        self.default_prefix = Some(default_prefix);
        self
    }

    #[allow(unused)]
    pub fn bookmark(mut self, bookmark: String) -> Self {
        self.source_bookmark = Some(bookmark);
        self
    }

    pub fn build_source(mut self) -> Result<SyncTargetConfigBuilder, Error> {
        let source_revision = match self.source_bookmark {
            Some(source_bookmark) => SourceRevision::bookmark(source_bookmark),
            None => {
                return Err(anyhow!("source bookmark not set"));
            }
        };

        let default_prefix = self
            .default_prefix
            .ok_or_else(|| anyhow!("default prefix is not set"))?;

        let source = Source {
            source_name: self.source_name,
            repo_id: self.repo_id.id() as i64,
            name: self.git_repo_name,
            revision: source_revision,
            mapping: SourceMappingRules {
                default_prefix,
                linkfiles: BTreeMap::new(),
                overrides: BTreeMap::new(),
            },
        };
        self.config_builder.add_source(source);
        Ok(self.config_builder)
    }
}
