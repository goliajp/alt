import {test,expect} from 'bun:test';
import {deviceFragment,normalizeDeviceCode} from './device-location';
test('device codes retain exact entropy and reject confusables',()=>{
  expect(normalizeDeviceCode(' abcd2345 ')).toBe('ABCD-2345');
  for(const code of ['ABCD-O234','ABCD-I234','ABCD-L234','ABCD-U234','AB-CD2345','ABCD23456','éééé','ßßßß'])expect(normalizeDeviceCode(code)).toBeNull();
});
test('only a single human code is accepted from the device fragment',()=>{
  expect(deviceFragment('#device?user_code=abcd-2345')).toEqual({code:'ABCD-2345'});
  expect(deviceFragment('#device')).toEqual({code:''});
  expect(deviceFragment('#device?user_code=ABCD2345&user_code=ZZZZ9999')).toEqual({code:''});
  expect(deviceFragment('#device?user_code=ABCD2345&device_code=secret')).toEqual({code:''});
  expect(deviceFragment('#devices?user_code=ABCD2345')).toBeNull();
  expect(deviceFragment('#repository=sample')).toBeNull();
});
