export const aboutEn = {
  aboutTitle:'About this instance',
  aboutWhat:'This is an alt code hosting instance. alt is a version control tool compatible with Git. Both Git and alt clients can clone from it and push to it.',
  aboutAddress:'Repository address',aboutRepositoryName:'repository',
  aboutAccess:'Access requires an account',
  aboutAccessBody:'Accounts are created by the instance administrator. Command-line access uses an access token, which the administrator provides.',
  aboutProject:'The alt project',aboutSource:'Source code: ',
};
type Copy = Record<keyof typeof aboutEn,string>;
export const aboutZh:Copy = {
  aboutTitle:'关于此实例',
  aboutWhat:'这是一个 alt 代码托管实例。alt 是与 Git 兼容的版本管理工具，用 Git 客户端或 alt 客户端都可以从这里克隆、向这里推送。',
  aboutAddress:'仓库地址',aboutRepositoryName:'仓库名',
  aboutAccess:'访问需要账号',
  aboutAccessBody:'账号由实例管理员创建。命令行访问使用访问令牌，令牌由管理员提供。',
  aboutProject:'alt 项目',aboutSource:'源码：',
};
export const aboutJa:Copy = {
  aboutTitle:'このインスタンスについて',
  aboutWhat:'ここは alt のコードホスティング用インスタンスです。alt は Git と互換性のあるバージョン管理ツールで、Git クライアントと alt クライアントのどちらからでもクローンとプッシュができます。',
  aboutAddress:'リポジトリのアドレス',aboutRepositoryName:'リポジトリ名',
  aboutAccess:'アクセスにはアカウントが必要です',
  aboutAccessBody:'アカウントはインスタンス管理者が作成します。コマンドラインからのアクセスにはアクセストークンを使い、トークンは管理者が提供します。',
  aboutProject:'alt プロジェクト',aboutSource:'ソースコード：',
};
