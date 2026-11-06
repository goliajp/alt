import { expect, test } from 'bun:test';
import { closeDocuments, documentKey, documentFromHash, documentHash, moveDocument, openDocument } from './documents';
test('documents preserve their original repository when the resource selector changes', () => {
  const first = openDocument([], { kind: 'blob', repository: 'one', oid: 'abc', path: 'readme' });
  const second = openDocument(first.documents, { kind: 'blob', repository: 'two', oid: 'abc', path: 'readme' });
  expect(second.documents).toHaveLength(2);
  expect(second.documents[0].document).toEqual({ kind: 'blob', repository: 'one', oid: 'abc', path: 'readme' });
  expect(openDocument(second.documents, first.documents[0].document).documents).toBe(second.documents);
  expect(closeDocuments(second.documents, second.key, [second.key]).active).toBe(first.key);
  expect(moveDocument(second.documents, first.key, 1)[1].key).toBe(first.key);
});
test('paths and request identities cannot collide through delimiters or epochs', () => {
  expect(documentKey({ kind: 'blob', repository: 'a:b', oid: 'c', path: 'd' })).not.toBe(documentKey({ kind: 'blob', repository: 'a', oid: 'b:c', path: 'd' }));
  expect(documentKey({ kind: 'review', repository: 'r', actor: 'a', request: 'q', epoch: '1' })).not.toBe(documentKey({ kind: 'review', repository: 'r', actor: 'a', request: 'q', epoch: '2' }));
});

test('shared document addresses preserve exact repository, request and epoch and reject executable or extra input', () => {
  const review = { kind: 'review' as const, repository: 'one', actor: 'bob', request: 'request-1', epoch: '9007199254740993' };
  expect(documentFromHash(documentHash(review))).toEqual(review);
  expect(documentFromHash('#document=' + encodeURIComponent(JSON.stringify({ ...review, token:'secret' })))).toBeUndefined();
  expect(documentFromHash('#document=' + encodeURIComponent(JSON.stringify({ ...review, repository:'../other' })))).toBeUndefined();
  expect(documentFromHash('#document=%')).toBeUndefined();
  expect(documentFromHash('#document=' + encodeURIComponent(JSON.stringify({ kind:'script', src:'javascript:alert(1)' })))).toBeUndefined();
});
test('migration documents stay bound to their original repository and carry no credentials',()=>{
  const document={kind:'migration' as const,repository:'source'};
  expect(documentFromHash(documentHash(document))).toEqual(document);
  expect(documentFromHash(documentHash({...document,token:'secret'} as typeof document))).toBeUndefined();
  expect(documentKey(document)).not.toBe(documentKey({...document,repository:'target'}));
});
