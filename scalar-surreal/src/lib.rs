use std::{borrow::Cow, marker::PhantomData, ops::Deref, sync::Arc};

use scalar::{
    db::{AuthenticationError, Credentials, DatabaseFactory},
    DateTime, Document, Item, Utc,
};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use surrealdb::{
    error::{Api, Db},
    opt::{
        auth::{Record, Root},
        IntoEndpoint, IntoQuery,
    },
    sql::Thing,
    Connection, Error, Surreal,
};

fn thing_to_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let t = Thing::deserialize(deserializer)?;
    Ok(t.id.to_raw())
}

#[derive(Clone)]
pub struct SurrealConnection<C: Connection> {
    namespace: String,
    db: String,
    inner: Surreal<C>,
}

impl<C: Connection> Deref for SurrealConnection<C> {
    type Target = Surreal<C>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SurrealItem<D> {
    #[serde(deserialize_with = "thing_to_string")]
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub inner: D,
}

impl<D> From<SurrealItem<D>> for Item<D> {
    fn from(item: SurrealItem<D>) -> Self {
        Self {
            id: item.id,
            created_at: item.created_at,
            modified_at: item.modified_at,
            published_at: item.published_at,
            inner: item.inner,
        }
    }
}

impl<D> From<Item<D>> for SurrealItem<D> {
    fn from(value: Item<D>) -> Self {
        Self {
            id: value.id,
            created_at: value.created_at,
            modified_at: value.modified_at,
            published_at: value.published_at,
            inner: value.inner,
        }
    }
}

pub struct SurrealStore<C: Connection, S, P: IntoEndpoint<S, Client = C> + Clone + Send + Sync> {
    endpoint: P,
    namespace: String,
    db: String,
    connection_marker: PhantomData<C>,
    scheme_marker: PhantomData<S>,
}

impl<C: Connection, S, P: IntoEndpoint<S, Client = C> + Clone + Send + Sync> Clone
    for SurrealStore<C, S, P>
{
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            namespace: self.namespace.clone(),
            db: self.db.clone(),
            connection_marker: PhantomData,
            scheme_marker: PhantomData,
        }
    }
}

impl<C: Connection, S, P: IntoEndpoint<S, Client = C> + Clone + Send + Sync> SurrealStore<C, S, P> {
    pub fn new(address: P, namespace: String, db: String) -> Self {
        Self {
            endpoint: address,
            namespace,
            db,
            connection_marker: PhantomData,
            scheme_marker: PhantomData,
        }
    }
}

impl<
        C: Connection + Clone,
        S: Send + Sync,
        P: IntoEndpoint<S, Client = C> + Clone + Send + Sync,
    > DatabaseFactory for SurrealStore<C, S, P>
{
    type Error = surrealdb::Error;

    type Connection = SurrealConnection<C>;

    async fn init(&self) -> Result<Self::Connection, Self::Error> {
        let inner = Surreal::new(self.endpoint.to_owned()).await?;

        inner.use_ns(&self.namespace).await?;
        inner.use_db(&self.db).await?;

        Ok(SurrealConnection {
            namespace: self.namespace.clone(),
            db: self.namespace.clone(),
            inner,
        })
    }

    async fn init_system(&self) -> Result<Self::Connection, Self::Error> {
        let inner = Surreal::new(self.endpoint.to_owned()).await?;

        inner.use_ns(&self.namespace).await?;
        inner.use_db(&self.db).await?;

        inner
            .signin(Root {
                username: "root",
                password: "root",
            })
            .await?;

        Ok(SurrealConnection {
            namespace: self.namespace.clone(),
            db: self.namespace.clone(),
            inner,
        })
    }
}

impl<C: Connection> Drop for SurrealConnection<C> {
    fn drop(&mut self) {
        println!("MEMORY LEAK PREVENTED!!!!");
    }
}

impl<C: Connection> scalar::DatabaseConnection for SurrealConnection<C> {
    type Error = surrealdb::Error;

    async fn draft<D: Document + Send>(
        &self,
        id: &str,
        data: serde_json::Value,
    ) -> Result<Item<serde_json::Value>, Self::Error> {
        #[derive(Serialize)]
        struct Bindings<'a> {
            doc: Cow<'a, str>,
            id: Cow<'a, str>,
            inner: serde_json::Value,
        }

        let mut result = self
            .query("LET $draft_id = type::thing(string::concat($doc, '_draft'), $id)")
            .query("LET $meta_id = type::thing(string::concat($doc, '_meta'), $id)")
            .query("UPSERT $draft_id SET inner = $inner")
            .query("UPSERT type::thing(string::concat($doc, '_meta'), $id) SET draft = $draft_id, modified_at = time::now()")
            .query(
                "SELECT
                id,
                created_at,
                modified_at,
                IF draft IS NOT NONE THEN draft.inner ELSE published.inner END AS inner,
                published.published_at AS published_at
            FROM $meta_id
            FETCH draft, published",
            )
            .bind(Bindings {
                doc: D::identifier().into(),
                id: id.to_owned().into(),
                inner: data,
            })
            .await?;

        let thingy: Option<SurrealItem<serde_json::Value>> =
            result.take(4).expect("this should always succeed");

        Ok(thingy
            .expect("this option should always return something")
            .into())
    }

    async fn delete_draft<D: Document + Send>(
        &self,
        id: &str,
    ) -> Result<Item<serde_json::Value>, Self::Error> {
        #[derive(Serialize)]
        struct Bindings<'a> {
            doc: Cow<'a, str>,
            id: Cow<'a, str>,
        }

        let result = self
            .query("LET $draft_id = type::thing(string::concat($doc, '_draft'), $id)")
            .query("LET $meta_id = type::thing(string::concat($doc, '_meta'), $id)")
            .query("DELETE $draft_id")
            .query("DELETE $meta_id WHERE published IS NONE")
            .bind(Bindings {
                doc: D::identifier().into(),
                id: id.to_owned().into(),
            });

        Ok(todo!())
    }

    async fn put<D: Document + Serialize + DeserializeOwned + Send + 'static>(
        &self,
        item: Item<D>,
    ) -> Result<Item<D>, Self::Error> {
        let updated_thingy: Option<SurrealItem<D>> = self
            .upsert((D::identifier(), item.id.to_owned()))
            .content(SurrealItem::<D>::from(item))
            .await?;

        Ok(updated_thingy
            .expect("surreal should return data regardless")
            .into())
    }

    async fn delete<D: Document + Send>(&self, id: &str) -> Result<Item<D>, Self::Error> {
        todo!()
    }

    async fn get_all<D: Document + DeserializeOwned + Send>(
        &self,
    ) -> Result<Vec<Item<serde_json::Value>>, Self::Error> {
        let result = self
            .query(
                "SELECT
                id,
                created_at,
                modified_at,
                IF draft IS NOT NONE THEN draft.inner ELSE published.inner END AS inner,
                published.published_at AS published_at
            FROM type::table(string::concat($doc, '_meta'))
            FETCH draft, published",
            )
            .bind(("doc", D::identifier()))
            .await?
            .take::<Vec<SurrealItem<serde_json::Value>>>(0)?;

        Ok(result.into_iter().map(Into::into).collect())
    }

    async fn get_by_id<D: Document + DeserializeOwned + Send>(
        &self,
        id: &str,
    ) -> Result<Option<Item<serde_json::Value>>, Self::Error> {
        #[derive(Serialize)]
        struct Bindings<'a> {
            doc: Cow<'a, str>,
            id: Cow<'a, str>,
        }

        Ok(self
            .query("LET $meta_id = type::thing(string::concat($doc, '_meta'), $id)")
            .query(
                "SELECT
                id,
                created_at,
                modified_at,
                IF draft IS NOT NONE THEN draft.inner ELSE published.inner END AS inner,
                published.published_at AS published_at
            FROM $meta_id
            FETCH draft, published",
            )
            .bind(Bindings {
                doc: D::identifier().into(),
                id: id.to_owned().into(),
            })
            .await?
            .take::<Option<SurrealItem<serde_json::Value>>>(1)?
            .map(Into::into))
    }

    async fn authenticate(&self, jwt: &str) -> Result<(), AuthenticationError<Self::Error>> {
        self.inner.authenticate(jwt).await.map_err(|e| match e {
            Error::Api(Api::Query(_)) => AuthenticationError::BadToken,
            Error::Db(Db::InvalidAuth) => AuthenticationError::BadToken,
            _ => e.into(),
        })?;

        Ok(())
    }

    async fn signin(
        &self,
        credentials: Credentials,
    ) -> Result<String, AuthenticationError<Self::Error>> {
        let result = self
            .inner
            .signin(Record {
                namespace: &self.namespace,
                database: &self.namespace,
                access: "sc__editor",
                params: credentials,
            })
            .await
            .map_err(|e| match e {
                Error::Api(Api::Query(_)) => AuthenticationError::BadCredentials,
                Error::Db(Db::InvalidAuth) => AuthenticationError::BadCredentials,
                _ => e.into(),
            })?;

        Ok(result.into_insecure_token())
    }
}

impl<C: Connection> SurrealConnection<C> {
    pub async fn init_doc<D: Document>(&self) {
        let published_table = D::identifier();
        let draft_table = format!("{published_table}_draft");
        let meta_table = format!("{published_table}_meta");
        self
            // published documents
            .query(format!("DEFINE TABLE OVERWRITE {published_table} SCHEMAFULL PERMISSIONS FOR select WHERE true FOR create, update, delete WHERE $auth.id IS NOT NONE"))
            .query(format!("DEFINE FIELD IF NOT EXISTS published_at ON {published_table} TYPE option<datetime>"))
            .query(format!("DEFINE FIELD IF NOT EXISTS inner ON {published_table} FLEXIBLE TYPE object"))
            // drafts
            .query(format!("DEFINE TABLE OVERWRITE {draft_table} SCHEMAFULL PERMISSIONS FOR select, create, update, delete WHERE $auth.id IS NOT NONE"))
            .query(format!("DEFINE FIELD IF NOT EXISTS inner ON {draft_table} FLEXIBLE TYPE object"))
            // meta table
            .query(format!("DEFINE TABLE OVERWRITE {meta_table} SCHEMAFULL PERMISSIONS FOR select, create, update, delete WHERE $auth.id IS NOT NONE"))
            .query(format!("DEFINE FIELD IF NOT EXISTS created_at ON {meta_table} TYPE datetime DEFAULT time::now()"))
            .query(format!("DEFINE FIELD IF NOT EXISTS modified_at ON {meta_table} TYPE datetime"))
            .query(format!("DEFINE FIELD IF NOT EXISTS draft ON {meta_table} TYPE option<record<{draft_table}>>"))
            .query(format!("DEFINE FIELD IF NOT EXISTS published ON {meta_table} TYPE option<record<{published_table}>>"))
            .await
            .expect(&format!("setting up tables for {published_table} failed"));
    }

    pub async fn init_auth(&self) {
        self
            .query("DEFINE TABLE OVERWRITE sc__editor SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE id = $auth.id OR $auth.admin = true FOR create WHERE $auth.admin = true")
            .query("DEFINE FIELD IF NOT EXISTS name ON sc__editor TYPE string")
            .query("DEFINE FIELD IF NOT EXISTS email ON sc__editor TYPE string ASSERT string::is::email($value)")
            .query("DEFINE FIELD IF NOT EXISTS password ON sc__editor TYPE string")
            .query("DEFINE FIELD IF NOT EXISTS admin ON sc__editor TYPE bool")
            .query("DEFINE INDEX email ON user FIELDS email UNIQUE")
            .query("
            DEFINE ACCESS OVERWRITE sc__editor ON DATABASE TYPE RECORD
            SIGNIN (
                SELECT * FROM sc__editor WHERE email = $email AND crypto::argon2::compare(password, $password)
            )
        ").await.expect("auth setup failed");
    }
}

// TODO: unit tests

#[macro_export]
macro_rules! doc_init {
    ($db:ident, $doc:ty) => {
        $db.init_doc::<$doc>().await;
    };
    ($db:ident, $doc:ty, $($docs:ty),+) => {
        ::scalar_surreal::doc_init!($db, $doc);
        ::scalar_surreal::doc_init!($db, $($docs),+);
    }
}

#[macro_export]
macro_rules! init {
    ($db:ident, $($docs:ty),+) => {
        $db.init_auth().await;
        ::scalar_surreal::doc_init!($db, $($docs),+);
    };
}