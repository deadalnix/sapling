/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use blobrepo::BlobRepo;
use cloned::cloned;
use context::CoreContext;
use futures::future;
use futures::stream::FuturesUnordered;
use futures::Stream;
use futures::StreamExt;
use futures::TryStreamExt;
use mercurial_bundles::obsmarkers::MetadataEntry;
use mercurial_bundles::part_encode::PartEncodeBuilder;
use mercurial_bundles::parts;
use mercurial_derived_data::DeriveHgChangeset;
use mercurial_types::HgChangesetId;
use mononoke_types::DateTime;

pub fn pushrebased_changesets_to_obsmarkers_part(
    ctx: CoreContext,
    blobrepo: &BlobRepo,
    pushrebased_changesets: Vec<pushrebase::PushrebaseChangesetPair>,
) -> Option<Result<PartEncodeBuilder>> {
    let filtered_changesets: Vec<_> = pushrebased_changesets
        .into_iter()
        .filter(|c| c.id_old != c.id_new)
        .collect();

    if filtered_changesets.is_empty() {
        return None;
    }

    let hg_pushrebased_changesets =
        pushrebased_changesets_to_hg_stream(ctx.clone(), blobrepo, filtered_changesets);

    let time = DateTime::now();
    let mut metadata = vec![MetadataEntry::new("operation", "push")];

    if let Some(user) = ctx.metadata().unix_name() {
        metadata.push(MetadataEntry::new("user", user));
    }

    let part = parts::obsmarkers_part(hg_pushrebased_changesets.boxed().compat(), time, metadata);

    Some(part)
}

fn pushrebased_changesets_to_hg_stream(
    ctx: CoreContext,
    blobrepo: &BlobRepo,
    pushrebased_changesets: Vec<pushrebase::PushrebaseChangesetPair>,
) -> impl Stream<Item = Result<(HgChangesetId, Vec<HgChangesetId>)>> {
    let blobrepo = blobrepo.clone();
    pushrebased_changesets
        .into_iter()
        .map(move |p| {
            cloned!(ctx, blobrepo);
            async move {
                let (old, new) = future::try_join(
                    blobrepo.derive_hg_changeset(&ctx, p.id_old),
                    blobrepo.derive_hg_changeset(&ctx, p.id_new),
                )
                .await?;
                Ok((old, vec![new]))
            }
        })
        .collect::<FuturesUnordered<_>>()
}
