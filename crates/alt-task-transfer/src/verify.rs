//! A package verified as one sent on, or as one received into a repository.

use crate::{Package, Result, package::verify_file};
use alt_git_codec::{ObjectId, ObjectKind};
use alt_repository_service::{ObjectReader, ObjectSource};
use alt_task_service::{TaskLimits, VerifiedPublication};

/// what tells the kind of an object a repository holds, where it does
pub type Held<'a> = dyn Fn(ObjectId) -> alt_repository_service::Result<Option<ObjectKind>> + 'a;

/// the objects of a package, settled where the repository it is received
/// into holds them
struct Into<'a> {
    package: &'a Package,
    held: &'a Held<'a>,
}
impl ObjectSource for Into<'_> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        ObjectSource::open(self.package, oid)
    }
    fn settled(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectKind>> {
        (self.held)(oid)
    }
}

impl Package {
    /// Verifies the package as one sent on: the server it is sent to tells
    /// what `git fsck` refuses of it, by what that repository holds.
    pub fn verify(&self, task_limits: TaskLimits) -> Result<VerifiedPublication> {
        self.files()?;
        let request = self.request();
        Ok(VerifiedPublication::verify_to_send(
            &request.publication,
            &request.manifest,
            self,
            self,
            self.limits,
            task_limits,
        )?)
    }
    /// Verifies the package as one received into a repository that holds the
    /// objects `held` tells the kind of. Of what `git fsck` refuses, what the
    /// package brings there anew is refused, and what the repository holds
    /// is not: no server stands between the package and the repository.
    pub fn verify_into(
        &self,
        task_limits: TaskLimits,
        held: &Held<'_>,
    ) -> Result<VerifiedPublication> {
        self.files()?;
        let request = self.request();
        let into = Into {
            package: self,
            held,
        };
        Ok(VerifiedPublication::verify(
            &request.publication,
            &request.manifest,
            &into,
            self,
            self.limits,
            task_limits,
        )?)
    }
    fn files(&self) -> Result<()> {
        for entry in &self.request().manifest.entries {
            verify_file(&self.directory.join("content").join(&entry.blake3), entry)?;
        }
        Ok(())
    }
}
