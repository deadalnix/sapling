/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashSet;

use async_trait::async_trait;
use futures::future::try_join_all;
use futures::try_join;
use itertools::Itertools;
use maplit::btreemap;
use mononoke_api::{
    ChangesetContext, ChangesetId, ChangesetPathContext, FileMetadata, FileType,
    HeaderlessUnifiedDiff, MononokeError, PushrebaseOutcome, RepoContext, TreeEntry, TreeId,
    TreeSummary, UnifiedDiff,
};
use source_control as thrift;
use std::collections::{BTreeMap, BTreeSet};

use crate::commit_id::{map_commit_identities, map_commit_identity};
use crate::errors;

/// Convert an item into a thrift type suitable for inclusion in a thrift
/// response.
pub(crate) trait IntoResponse<T> {
    fn into_response(self) -> T;
}

/// Asynchronously convert an item into a thrift type suitable for inclusion
/// in a thrift response.
#[async_trait]
pub(crate) trait AsyncIntoResponse<T> {
    async fn into_response(self) -> Result<T, errors::ServiceError>;
}

/// Asynchronously convert an item into a thrift type suitable for inclusion
/// in a thrift response, with additional data required for the conversion.
#[async_trait]
pub(crate) trait AsyncIntoResponseWith<T> {
    /// The type of additional data that must be provided to convert this
    /// value into a response value.
    type Additional;

    async fn into_response_with(
        self,
        additional: &Self::Additional,
    ) -> Result<T, errors::ServiceError>;
}

impl IntoResponse<thrift::EntryType> for FileType {
    fn into_response(self) -> thrift::EntryType {
        match self {
            FileType::Regular => thrift::EntryType::FILE,
            FileType::Executable => thrift::EntryType::EXEC,
            FileType::Symlink => thrift::EntryType::LINK,
        }
    }
}

impl IntoResponse<thrift::TreeEntry> for (String, TreeEntry) {
    fn into_response(self) -> thrift::TreeEntry {
        let (name, entry) = self;
        let (type_, info) = match entry {
            TreeEntry::Directory(dir) => {
                let summary = dir.summary();
                let info = thrift::TreeInfo {
                    id: dir.id().as_ref().to_vec(),
                    simple_format_sha1: summary.simple_format_sha1.as_ref().to_vec(),
                    simple_format_sha256: summary.simple_format_sha256.as_ref().to_vec(),
                    child_files_count: summary.child_files_count as i64,
                    child_files_total_size: summary.child_files_total_size as i64,
                    child_dirs_count: summary.child_dirs_count as i64,
                    descendant_files_count: summary.descendant_files_count as i64,
                    descendant_files_total_size: summary.descendant_files_total_size as i64,
                };
                (thrift::EntryType::TREE, thrift::EntryInfo::tree(info))
            }
            TreeEntry::File(file) => {
                let info = thrift::FileInfo {
                    id: file.content_id().as_ref().to_vec(),
                    file_size: file.size() as i64,
                    content_sha1: file.content_sha1().as_ref().to_vec(),
                    content_sha256: file.content_sha256().as_ref().to_vec(),
                };
                (
                    file.file_type().into_response(),
                    thrift::EntryInfo::file(info),
                )
            }
        };
        thrift::TreeEntry { name, type_, info }
    }
}

impl IntoResponse<thrift::FileInfo> for FileMetadata {
    fn into_response(self) -> thrift::FileInfo {
        thrift::FileInfo {
            id: self.content_id.as_ref().to_vec(),
            file_size: self.total_size as i64,
            content_sha1: self.sha1.as_ref().to_vec(),
            content_sha256: self.sha256.as_ref().to_vec(),
        }
    }
}

impl IntoResponse<thrift::TreeInfo> for (TreeId, TreeSummary) {
    fn into_response(self) -> thrift::TreeInfo {
        let (id, summary) = self;
        thrift::TreeInfo {
            id: id.as_ref().to_vec(),
            simple_format_sha1: summary.simple_format_sha1.as_ref().to_vec(),
            simple_format_sha256: summary.simple_format_sha256.as_ref().to_vec(),
            child_files_count: summary.child_files_count as i64,
            child_files_total_size: summary.child_files_total_size as i64,
            child_dirs_count: summary.child_dirs_count as i64,
            descendant_files_count: summary.descendant_files_count as i64,
            descendant_files_total_size: summary.descendant_files_total_size as i64,
        }
    }
}

impl IntoResponse<thrift::Diff> for UnifiedDiff {
    fn into_response(self) -> thrift::Diff {
        thrift::Diff::raw_diff(thrift::RawDiff {
            raw_diff: Some(self.raw_diff),
            is_binary: self.is_binary,
        })
    }
}

impl IntoResponse<thrift::Diff> for HeaderlessUnifiedDiff {
    fn into_response(self) -> thrift::Diff {
        thrift::Diff::raw_diff(thrift::RawDiff {
            raw_diff: Some(self.raw_diff),
            is_binary: self.is_binary,
        })
    }
}

#[async_trait]
impl AsyncIntoResponse<Option<thrift::FilePathInfo>> for ChangesetPathContext {
    async fn into_response(self) -> Result<Option<thrift::FilePathInfo>, errors::ServiceError> {
        let (meta, type_) = try_join!(
            async {
                let file = self.file().await?;
                match file {
                    Some(file) => Ok(Some(file.metadata().await?)),
                    None => Ok(None),
                }
            },
            self.file_type()
        )?;
        if let (Some(meta), Some(type_)) = (meta, type_) {
            Ok(Some(thrift::FilePathInfo {
                path: self.path().to_string(),
                type_: type_.into_response(),
                info: meta.into_response(),
            }))
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl AsyncIntoResponse<Option<thrift::TreePathInfo>> for ChangesetPathContext {
    async fn into_response(self) -> Result<Option<thrift::TreePathInfo>, errors::ServiceError> {
        let tree = self.tree().await?;
        let summary = match tree {
            Some(tree) => Some((tree.id().clone(), tree.summary().await?)),
            None => None,
        };
        if let Some(summary) = summary {
            Ok(Some(thrift::TreePathInfo {
                path: self.path().to_string(),
                info: summary.into_response(),
            }))
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl AsyncIntoResponseWith<thrift::CommitInfo> for ChangesetContext {
    /// The additional data is the set of commit identity schemes to be
    /// returned in the response.
    type Additional = BTreeSet<thrift::CommitIdentityScheme>;

    async fn into_response_with(
        self,
        identity_schemes: &BTreeSet<thrift::CommitIdentityScheme>,
    ) -> Result<thrift::CommitInfo, errors::ServiceError> {
        async fn map_parent_identities(
            changeset: &ChangesetContext,
            identity_schemes: &BTreeSet<thrift::CommitIdentityScheme>,
        ) -> Result<Vec<BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>, MononokeError>
        {
            let parents = changeset.parents().await?;
            let parent_id_mapping =
                map_commit_identities(changeset.repo(), parents.clone(), identity_schemes).await?;
            Ok(parents
                .iter()
                .map(|parent_id| {
                    parent_id_mapping
                        .get(parent_id)
                        .map(Clone::clone)
                        .unwrap_or_else(BTreeMap::new)
                })
                .collect())
        }

        let (ids, message, date, author, parents, extra, generation) = try_join!(
            map_commit_identity(&self, identity_schemes),
            self.message(),
            self.author_date(),
            self.author(),
            map_parent_identities(&self, identity_schemes),
            self.extras(),
            self.generation(),
        )?;
        Ok(thrift::CommitInfo {
            ids,
            message,
            date: date.timestamp(),
            tz: date.offset().local_minus_utc(),
            author,
            parents,
            extra: extra.into_iter().collect(),
            generation: generation.value() as i64,
        })
    }
}

#[async_trait]
impl AsyncIntoResponseWith<Vec<BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>>
    for Vec<ChangesetContext>
{
    /// The additional data is the set of commit identity schemes to be
    /// returned in the response.
    type Additional = BTreeSet<thrift::CommitIdentityScheme>;

    async fn into_response_with(
        self,
        identity_schemes: &BTreeSet<thrift::CommitIdentityScheme>,
    ) -> Result<Vec<BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>, errors::ServiceError>
    {
        let res = try_join_all({
            let changesets_grouped_by_repo = self
                .into_iter()
                .map(|c| c.into_repo_and_id())
                .into_group_map();

            changesets_grouped_by_repo
                .into_iter()
                .map(|(repo, changesets)| async move {
                    let id_map =
                        map_commit_identities(&repo, changesets.clone(), identity_schemes).await?;

                    changesets
                        .iter()
                        .map(move |id| {
                            id_map.get(id).cloned().ok_or_else(|| {
                                errors::internal_error(
                                    "programming error, id is missing from the map",
                                )
                                .into()
                            })
                        })
                        .collect::<Result<
                            Vec<BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>,
                            errors::ServiceError,
                        >>()
                })
        })
        .await?
        .into_iter()
        .flatten()
        .collect();
        Ok(res)
    }
}

#[async_trait]
impl AsyncIntoResponseWith<thrift::PushrebaseOutcome> for PushrebaseOutcome {
    /// The additional data is the repo context, the set of commit identity
    /// schemes to be returned in the response, and optionally a different set
    /// of commit identity schemes to use for the old commit ids.
    type Additional = (
        RepoContext,
        BTreeSet<thrift::CommitIdentityScheme>,
        Option<BTreeSet<thrift::CommitIdentityScheme>>,
    );

    async fn into_response_with(
        self,
        additional: &Self::Additional,
    ) -> Result<thrift::PushrebaseOutcome, errors::ServiceError> {
        let (repo, identity_schemes, old_identity_schemes) = additional;
        let mut new_ids = HashSet::new();
        let mut old_ids = HashSet::new();
        new_ids.insert(self.head);
        for rebase in self.rebased_changesets.iter() {
            old_ids.insert(rebase.id_old);
            new_ids.insert(rebase.id_new);
        }
        let old_identity_schemes = old_identity_schemes.as_ref().unwrap_or(&identity_schemes);
        let (old_id_map, new_id_map) = try_join!(
            map_commit_identities(&repo, old_ids.into_iter().collect(), old_identity_schemes),
            map_commit_identities(&repo, new_ids.into_iter().collect(), &identity_schemes),
        )?;

        // Map IDs using one of the maps we just fetched.  If we couldn't
        // perform the look-up then just return the bonsai ID only.
        fn try_get(
            map: &BTreeMap<ChangesetId, BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId>>,
            cs_id: ChangesetId,
        ) -> BTreeMap<thrift::CommitIdentityScheme, thrift::CommitId> {
            match map.get(&cs_id) {
                Some(ids) => ids.clone(),
                None => btreemap! {
                    thrift::CommitIdentityScheme::BONSAI =>
                        thrift::CommitId::bonsai(cs_id.as_ref().into()),
                },
            }
        }

        let head = try_get(&new_id_map, self.head);
        let rebased_commits: Vec<_> = self
            .rebased_changesets
            .iter()
            .map(|rebase| {
                let old_ids = try_get(&old_id_map, rebase.id_old);
                let new_ids = try_get(&new_id_map, rebase.id_new);
                thrift::PushrebaseRebasedCommit { old_ids, new_ids }
            })
            .collect();

        Ok(thrift::PushrebaseOutcome {
            head,
            rebased_commits,
        })
    }
}
