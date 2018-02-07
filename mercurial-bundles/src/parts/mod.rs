// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use bytes::Bytes;
use failure::err_msg;
use futures::{Future, Stream};
use futures::stream::{iter_ok, once};

use super::changegroup::{CgDeltaChunk, Part, Section};
use super::changegroup::packer::Cg2Packer;
use super::wirepack;
use super::wirepack::packer::WirePackPacker;

use errors::*;
use mercurial_types::{BlobNode, Delta, MPath, NodeHash, RepoPath, NULL_HASH};
use mercurial_types::manifest::Entry;
use part_encode::PartEncodeBuilder;
use part_header::PartHeaderType;

pub fn listkey_part<N, S, K, V>(namespace: N, items: S) -> Result<PartEncodeBuilder>
where
    N: Into<Bytes>,
    S: Stream<Item = (K, V), Error = Error> + Send + 'static,
    K: AsRef<[u8]>,
    V: AsRef<[u8]>,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::Listkeys)?;
    builder.add_mparam("namespace", namespace)?;
    // Ideally we'd use a size_hint here, but streams don't appear to have one.
    let payload = Vec::with_capacity(256);
    let fut = items
        .fold(payload, |mut payload, (key, value)| {
            payload.extend_from_slice(key.as_ref());
            payload.push(b'\t');
            payload.extend_from_slice(value.as_ref());
            payload.push(b'\n');
            Ok::<_, Error>(payload)
        })
        .map_err(|err| Error::from(err.context(ErrorKind::ListkeyGeneration)));

    builder.set_data_future(fut);

    Ok(builder)
}

pub fn changegroup_part<S>(changelogentries: S) -> Result<PartEncodeBuilder>
where
    S: Stream<Item = BlobNode, Error = Error> + Send + 'static,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::Changegroup)?;
    builder.add_mparam("version", "02")?;

    let changelogentries = changelogentries.map(|blobnode| {
        let node = blobnode.nodeid().expect("blobnode should store data");
        let parents = blobnode.parents().get_nodes();
        let p1 = *parents.0.unwrap_or(&NULL_HASH);
        let p2 = *parents.1.unwrap_or(&NULL_HASH);
        let base = NULL_HASH;
        // Linknode is the same as node
        let linknode = node;
        let text = blobnode.as_blob().as_inner().unwrap_or(&vec![]).clone();
        let delta = Delta::new_fulltext(text);

        let deltachunk = CgDeltaChunk {
            node,
            p1,
            p2,
            base,
            linknode,
            delta,
        };
        Part::CgChunk(Section::Changeset, deltachunk)
    });

    let changelogentries = changelogentries
        .chain(once(Ok(Part::SectionEnd(Section::Changeset))))
        // One more SectionEnd entry is necessary because hg client excepts filelog section
        // even if it's empty. Add SectionEnd part with a fake file name
        .chain(once(Ok(Part::SectionEnd(Section::Filelog(MPath::empty())))))
        .chain(once(Ok(Part::End)));

    let cgdata = Cg2Packer::new(changelogentries);
    builder.set_data_generated(cgdata);

    Ok(builder)
}

pub fn treepack_part<S>(entries: S) -> Result<PartEncodeBuilder>
where
    S: Stream<Item = (Box<Entry + Sync>, NodeHash, MPath), Error = Error> + Send + 'static,
{
    let mut builder = PartEncodeBuilder::mandatory(PartHeaderType::B2xTreegroup2)?;
    builder.add_mparam("version", "1")?;
    builder.add_mparam("cache", "True")?;
    builder.add_mparam("category", "manifests")?;

    let wirepack_parts = entries
        .and_then(|(entry, linknode, basepath)| {
            entry
                .get_raw_content()
                .and_then(|blob| blob.into_inner().ok_or(err_msg("bad blob content")))
                .map(move |content| (entry, content, linknode, basepath))
        })
        .and_then(|(entry, content, linknode, basepath)| {
            entry
                .get_parents()
                .map(move |parents| (entry, parents, content, linknode, basepath))
        })
        .and_then(|(entry, parents, content, linknode, basepath)| {
            let path = basepath.clone().join(&entry.get_mpath());
            let path = if path.is_empty() {
                Ok(RepoPath::RootPath)
            } else {
                RepoPath::dir(path)
            };
            path.map(|path| (entry, parents, content, linknode, path))
        })
        .map(|(entry, parents, content, linknode, path)| {
            let history_meta = wirepack::Part::HistoryMeta {
                path: path.clone(),
                entry_count: 1,
            };

            let node: NodeHash = entry.get_hash().into_nodehash();
            let (p1, p2) = parents.get_nodes();
            let p1 = *p1.unwrap_or(&NULL_HASH);
            let p2 = *p2.unwrap_or(&NULL_HASH);

            let history = wirepack::Part::History(wirepack::HistoryEntry {
                node: node.clone(),
                p1,
                p2,
                linknode,
                // No copies/renames for trees
                copy_from: None,
            });

            let data_meta = wirepack::Part::DataMeta {
                path,
                entry_count: 1,
            };

            let data = wirepack::Part::Data(wirepack::DataEntry {
                node,
                delta_base: NULL_HASH,
                delta: Delta::new_fulltext(content),
            });

            iter_ok(vec![history_meta, history, data_meta, data].into_iter())
        })
        .flatten()
        .chain(once(Ok(wirepack::Part::End)));

    let packer = WirePackPacker::new(wirepack_parts, wirepack::Kind::Tree);
    builder.set_data_generated(packer);

    Ok(builder)
}
