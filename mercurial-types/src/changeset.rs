// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::BTreeMap;

use mononoke_types::{DateTime, MPath};

use blobnode::DParents;
use nodehash::DManifestId;

pub trait Changeset: Send + 'static {
    fn manifestid(&self) -> &DManifestId;
    fn user(&self) -> &[u8];
    fn extra(&self) -> &BTreeMap<Vec<u8>, Vec<u8>>;
    fn comments(&self) -> &[u8];
    fn files(&self) -> &[MPath];
    fn time(&self) -> &DateTime;
    fn parents(&self) -> &DParents;

    fn boxed(self) -> Box<Changeset>
    where
        Self: Sized,
    {
        Box::new(self)
    }
}

impl Changeset for Box<Changeset> {
    fn manifestid(&self) -> &DManifestId {
        (**self).manifestid()
    }

    fn user(&self) -> &[u8] {
        (**self).user()
    }

    fn extra(&self) -> &BTreeMap<Vec<u8>, Vec<u8>> {
        (**self).extra()
    }

    fn comments(&self) -> &[u8] {
        (**self).comments()
    }

    fn files(&self) -> &[MPath] {
        (**self).files()
    }

    fn time(&self) -> &DateTime {
        (**self).time()
    }

    fn parents(&self) -> &DParents {
        (**self).parents()
    }
}
