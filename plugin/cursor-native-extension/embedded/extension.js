"use strict";
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __esm = (fn, res, err) => function __init() {
  if (err) throw err[0];
  try {
    return fn && (res = (0, fn[__getOwnPropNames(fn)[0]])(fn = 0)), res;
  } catch (e) {
    throw err = [e], e;
  }
};
var __commonJS = (cb, mod) => function __require() {
  try {
    return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
  } catch (e) {
    throw mod = 0, e;
  }
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// node_modules/vscode-languageclient/lib/common/utils/is.js
var require_is = __commonJS({
  "node_modules/vscode-languageclient/lib/common/utils/is.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.boolean = boolean;
    exports2.string = string;
    exports2.number = number;
    exports2.error = error;
    exports2.func = func;
    exports2.array = array;
    exports2.stringArray = stringArray;
    exports2.typedArray = typedArray;
    exports2.thenable = thenable;
    exports2.asPromise = asPromise;
    function boolean(value) {
      return value === true || value === false;
    }
    function string(value) {
      return typeof value === "string" || value instanceof String;
    }
    function number(value) {
      return typeof value === "number" || value instanceof Number;
    }
    function error(value) {
      return value instanceof Error;
    }
    function func(value) {
      return typeof value === "function";
    }
    function array(value) {
      return Array.isArray(value);
    }
    function stringArray(value) {
      return array(value) && value.every((elem) => string(elem));
    }
    function typedArray(value, check) {
      return Array.isArray(value) && value.every(check);
    }
    function thenable(value) {
      return value && func(value.then);
    }
    function asPromise(value) {
      if (value instanceof Promise) {
        return value;
      } else if (thenable(value)) {
        return new Promise((resolve, reject) => {
          value.then((resolved) => resolve(resolved), (error2) => reject(error2));
        });
      } else {
        return Promise.resolve(value);
      }
    }
  }
});

// node_modules/vscode-jsonrpc/lib/common/is.js
var require_is2 = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/is.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.boolean = boolean;
    exports2.string = string;
    exports2.number = number;
    exports2.error = error;
    exports2.func = func;
    exports2.array = array;
    exports2.stringArray = stringArray;
    function boolean(value) {
      return value === true || value === false;
    }
    function string(value) {
      return typeof value === "string" || value instanceof String;
    }
    function number(value) {
      return typeof value === "number" || value instanceof Number;
    }
    function error(value) {
      return value instanceof Error;
    }
    function func(value) {
      return typeof value === "function";
    }
    function array(value) {
      return Array.isArray(value);
    }
    function stringArray(value) {
      return array(value) && value.every((elem) => string(elem));
    }
  }
});

// node_modules/vscode-jsonrpc/lib/common/messages.js
var require_messages = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/messages.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.Message = exports2.NotificationType9 = exports2.NotificationType8 = exports2.NotificationType7 = exports2.NotificationType6 = exports2.NotificationType5 = exports2.NotificationType4 = exports2.NotificationType3 = exports2.NotificationType2 = exports2.NotificationType1 = exports2.NotificationType0 = exports2.NotificationType = exports2.RequestType9 = exports2.RequestType8 = exports2.RequestType7 = exports2.RequestType6 = exports2.RequestType5 = exports2.RequestType4 = exports2.RequestType3 = exports2.RequestType2 = exports2.RequestType1 = exports2.RequestType = exports2.RequestType0 = exports2.AbstractMessageSignature = exports2.ParameterStructures = exports2.ResponseError = exports2.ErrorCodes = void 0;
    var is = __importStar(require_is2());
    var ErrorCodes;
    (function(ErrorCodes2) {
      ErrorCodes2.ParseError = -32700;
      ErrorCodes2.InvalidRequest = -32600;
      ErrorCodes2.MethodNotFound = -32601;
      ErrorCodes2.InvalidParams = -32602;
      ErrorCodes2.InternalError = -32603;
      ErrorCodes2.jsonrpcReservedErrorRangeStart = -32099;
      ErrorCodes2.serverErrorStart = -32099;
      ErrorCodes2.MessageWriteError = -32099;
      ErrorCodes2.MessageReadError = -32098;
      ErrorCodes2.PendingResponseRejected = -32097;
      ErrorCodes2.ConnectionInactive = -32096;
      ErrorCodes2.ServerNotInitialized = -32002;
      ErrorCodes2.UnknownErrorCode = -32001;
      ErrorCodes2.jsonrpcReservedErrorRangeEnd = -32e3;
      ErrorCodes2.serverErrorEnd = -32e3;
    })(ErrorCodes || (exports2.ErrorCodes = ErrorCodes = {}));
    var ResponseError = class _ResponseError extends Error {
      code;
      data;
      constructor(code, message, data) {
        super(message);
        this.code = is.number(code) ? code : ErrorCodes.UnknownErrorCode;
        this.data = data;
        Object.setPrototypeOf(this, _ResponseError.prototype);
      }
      toJson() {
        const result = {
          code: this.code,
          message: this.message
        };
        if (this.data !== void 0) {
          result.data = this.data;
        }
        return result;
      }
    };
    exports2.ResponseError = ResponseError;
    var ParameterStructures = class _ParameterStructures {
      kind;
      /**
       * The parameter structure is automatically inferred on the number of parameters
       * and the parameter type in case of a single param.
       */
      static auto = new _ParameterStructures("auto");
      /**
       * Forces `byPosition` parameter structure. This is useful if you have a single
       * parameter which has a literal type.
       */
      static byPosition = new _ParameterStructures("byPosition");
      /**
       * Forces `byName` parameter structure. This is only useful when having a single
       * parameter. The library will report errors if used with a different number of
       * parameters.
       */
      static byName = new _ParameterStructures("byName");
      constructor(kind) {
        this.kind = kind;
      }
      static is(value) {
        return value === _ParameterStructures.auto || value === _ParameterStructures.byName || value === _ParameterStructures.byPosition;
      }
      toString() {
        return this.kind;
      }
    };
    exports2.ParameterStructures = ParameterStructures;
    var AbstractMessageSignature = class {
      method;
      numberOfParams;
      constructor(method, numberOfParams) {
        this.method = method;
        this.numberOfParams = numberOfParams;
      }
      get parameterStructures() {
        return ParameterStructures.auto;
      }
    };
    exports2.AbstractMessageSignature = AbstractMessageSignature;
    var RequestType0 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 0);
      }
    };
    exports2.RequestType0 = RequestType0;
    var RequestType = class extends AbstractMessageSignature {
      _parameterStructures;
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method, _parameterStructures = ParameterStructures.auto) {
        super(method, 1);
        this._parameterStructures = _parameterStructures;
      }
      get parameterStructures() {
        return this._parameterStructures;
      }
    };
    exports2.RequestType = RequestType;
    var RequestType1 = class extends AbstractMessageSignature {
      _parameterStructures;
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method, _parameterStructures = ParameterStructures.auto) {
        super(method, 1);
        this._parameterStructures = _parameterStructures;
      }
      get parameterStructures() {
        return this._parameterStructures;
      }
    };
    exports2.RequestType1 = RequestType1;
    var RequestType2 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 2);
      }
    };
    exports2.RequestType2 = RequestType2;
    var RequestType3 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 3);
      }
    };
    exports2.RequestType3 = RequestType3;
    var RequestType4 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 4);
      }
    };
    exports2.RequestType4 = RequestType4;
    var RequestType5 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 5);
      }
    };
    exports2.RequestType5 = RequestType5;
    var RequestType6 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 6);
      }
    };
    exports2.RequestType6 = RequestType6;
    var RequestType7 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 7);
      }
    };
    exports2.RequestType7 = RequestType7;
    var RequestType8 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 8);
      }
    };
    exports2.RequestType8 = RequestType8;
    var RequestType9 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 9);
      }
    };
    exports2.RequestType9 = RequestType9;
    var NotificationType = class extends AbstractMessageSignature {
      _parameterStructures;
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method, _parameterStructures = ParameterStructures.auto) {
        super(method, 1);
        this._parameterStructures = _parameterStructures;
      }
      get parameterStructures() {
        return this._parameterStructures;
      }
    };
    exports2.NotificationType = NotificationType;
    var NotificationType0 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 0);
      }
    };
    exports2.NotificationType0 = NotificationType0;
    var NotificationType1 = class extends AbstractMessageSignature {
      _parameterStructures;
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method, _parameterStructures = ParameterStructures.auto) {
        super(method, 1);
        this._parameterStructures = _parameterStructures;
      }
      get parameterStructures() {
        return this._parameterStructures;
      }
    };
    exports2.NotificationType1 = NotificationType1;
    var NotificationType2 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 2);
      }
    };
    exports2.NotificationType2 = NotificationType2;
    var NotificationType3 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 3);
      }
    };
    exports2.NotificationType3 = NotificationType3;
    var NotificationType4 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 4);
      }
    };
    exports2.NotificationType4 = NotificationType4;
    var NotificationType5 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 5);
      }
    };
    exports2.NotificationType5 = NotificationType5;
    var NotificationType6 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 6);
      }
    };
    exports2.NotificationType6 = NotificationType6;
    var NotificationType7 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 7);
      }
    };
    exports2.NotificationType7 = NotificationType7;
    var NotificationType8 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 8);
      }
    };
    exports2.NotificationType8 = NotificationType8;
    var NotificationType9 = class extends AbstractMessageSignature {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      _;
      constructor(method) {
        super(method, 9);
      }
    };
    exports2.NotificationType9 = NotificationType9;
    var Message;
    (function(Message2) {
      function isRequest(message) {
        const candidate = message;
        return candidate && is.string(candidate.method) && (is.string(candidate.id) || is.number(candidate.id));
      }
      Message2.isRequest = isRequest;
      function isNotification(message) {
        const candidate = message;
        return candidate && is.string(candidate.method) && message.id === void 0;
      }
      Message2.isNotification = isNotification;
      function isResponse(message) {
        const candidate = message;
        return candidate && (candidate.result !== void 0 || !!candidate.error) && (is.string(candidate.id) || is.number(candidate.id) || candidate.id === null);
      }
      Message2.isResponse = isResponse;
    })(Message || (exports2.Message = Message = {}));
  }
});

// node_modules/vscode-jsonrpc/lib/common/linkedMap.js
var require_linkedMap = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/linkedMap.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.LRUCache = exports2.LinkedMap = exports2.Touch = void 0;
    var Touch;
    (function(Touch2) {
      Touch2.None = 0;
      Touch2.First = 1;
      Touch2.AsOld = Touch2.First;
      Touch2.Last = 2;
      Touch2.AsNew = Touch2.Last;
    })(Touch || (exports2.Touch = Touch = {}));
    var LinkedMap = class {
      [Symbol.toStringTag] = "LinkedMap";
      _map;
      _head;
      _tail;
      _size;
      _state;
      constructor() {
        this._map = /* @__PURE__ */ new Map();
        this._head = void 0;
        this._tail = void 0;
        this._size = 0;
        this._state = 0;
      }
      clear() {
        this._map.clear();
        this._head = void 0;
        this._tail = void 0;
        this._size = 0;
        this._state++;
      }
      isEmpty() {
        return !this._head && !this._tail;
      }
      get size() {
        return this._size;
      }
      get first() {
        return this._head?.value;
      }
      get last() {
        return this._tail?.value;
      }
      before(key) {
        const item = this._map.get(key);
        return item ? item.previous?.value : void 0;
      }
      after(key) {
        const item = this._map.get(key);
        return item ? item.next?.value : void 0;
      }
      has(key) {
        return this._map.has(key);
      }
      get(key, touch = Touch.None) {
        const item = this._map.get(key);
        if (!item) {
          return void 0;
        }
        if (touch !== Touch.None) {
          this.touch(item, touch);
        }
        return item.value;
      }
      set(key, value, touch = Touch.None) {
        let item = this._map.get(key);
        if (item) {
          item.value = value;
          if (touch !== Touch.None) {
            this.touch(item, touch);
          }
        } else {
          item = { key, value, next: void 0, previous: void 0 };
          switch (touch) {
            case Touch.None:
              this.addItemLast(item);
              break;
            case Touch.First:
              this.addItemFirst(item);
              break;
            case Touch.Last:
              this.addItemLast(item);
              break;
            default:
              this.addItemLast(item);
              break;
          }
          this._map.set(key, item);
          this._size++;
        }
        return this;
      }
      delete(key) {
        return !!this.remove(key);
      }
      remove(key) {
        const item = this._map.get(key);
        if (!item) {
          return void 0;
        }
        this._map.delete(key);
        this.removeItem(item);
        this._size--;
        return item.value;
      }
      shift() {
        if (!this._head && !this._tail) {
          return void 0;
        }
        if (!this._head || !this._tail) {
          throw new Error("Invalid list");
        }
        const item = this._head;
        this._map.delete(item.key);
        this.removeItem(item);
        this._size--;
        return item.value;
      }
      forEach(callbackfn, thisArg) {
        const state = this._state;
        let current = this._head;
        while (current) {
          if (thisArg) {
            callbackfn.bind(thisArg)(current.value, current.key, this);
          } else {
            callbackfn(current.value, current.key, this);
          }
          if (this._state !== state) {
            throw new Error(`LinkedMap got modified during iteration.`);
          }
          current = current.next;
        }
      }
      keys() {
        const state = this._state;
        let current = this._head;
        const iterator = {
          [Symbol.iterator]: () => {
            return iterator;
          },
          next: () => {
            if (this._state !== state) {
              throw new Error(`LinkedMap got modified during iteration.`);
            }
            if (current) {
              const result = { value: current.key, done: false };
              current = current.next;
              return result;
            } else {
              return { value: void 0, done: true };
            }
          }
        };
        return iterator;
      }
      values() {
        const state = this._state;
        let current = this._head;
        const iterator = {
          [Symbol.iterator]: () => {
            return iterator;
          },
          next: () => {
            if (this._state !== state) {
              throw new Error(`LinkedMap got modified during iteration.`);
            }
            if (current) {
              const result = { value: current.value, done: false };
              current = current.next;
              return result;
            } else {
              return { value: void 0, done: true };
            }
          }
        };
        return iterator;
      }
      entries() {
        const state = this._state;
        let current = this._head;
        const iterator = {
          [Symbol.iterator]: () => {
            return iterator;
          },
          next: () => {
            if (this._state !== state) {
              throw new Error(`LinkedMap got modified during iteration.`);
            }
            if (current) {
              const result = { value: [current.key, current.value], done: false };
              current = current.next;
              return result;
            } else {
              return { value: void 0, done: true };
            }
          }
        };
        return iterator;
      }
      [Symbol.iterator]() {
        return this.entries();
      }
      trimOld(newSize) {
        if (newSize >= this.size) {
          return;
        }
        if (newSize === 0) {
          this.clear();
          return;
        }
        let current = this._head;
        let currentSize = this.size;
        while (current && currentSize > newSize) {
          this._map.delete(current.key);
          current = current.next;
          currentSize--;
        }
        this._head = current;
        this._size = currentSize;
        if (current) {
          current.previous = void 0;
        }
        this._state++;
      }
      addItemFirst(item) {
        if (!this._head && !this._tail) {
          this._tail = item;
        } else if (!this._head) {
          throw new Error("Invalid list");
        } else {
          item.next = this._head;
          this._head.previous = item;
        }
        this._head = item;
        this._state++;
      }
      addItemLast(item) {
        if (!this._head && !this._tail) {
          this._head = item;
        } else if (!this._tail) {
          throw new Error("Invalid list");
        } else {
          item.previous = this._tail;
          this._tail.next = item;
        }
        this._tail = item;
        this._state++;
      }
      removeItem(item) {
        if (item === this._head && item === this._tail) {
          this._head = void 0;
          this._tail = void 0;
        } else if (item === this._head) {
          if (!item.next) {
            throw new Error("Invalid list");
          }
          item.next.previous = void 0;
          this._head = item.next;
        } else if (item === this._tail) {
          if (!item.previous) {
            throw new Error("Invalid list");
          }
          item.previous.next = void 0;
          this._tail = item.previous;
        } else {
          const next = item.next;
          const previous = item.previous;
          if (!next || !previous) {
            throw new Error("Invalid list");
          }
          next.previous = previous;
          previous.next = next;
        }
        item.next = void 0;
        item.previous = void 0;
        this._state++;
      }
      touch(item, touch) {
        if (!this._head || !this._tail) {
          throw new Error("Invalid list");
        }
        if (touch !== Touch.First && touch !== Touch.Last) {
          return;
        }
        if (touch === Touch.First) {
          if (item === this._head) {
            return;
          }
          const next = item.next;
          const previous = item.previous;
          if (item === this._tail) {
            previous.next = void 0;
            this._tail = previous;
          } else {
            next.previous = previous;
            previous.next = next;
          }
          item.previous = void 0;
          item.next = this._head;
          this._head.previous = item;
          this._head = item;
          this._state++;
        } else if (touch === Touch.Last) {
          if (item === this._tail) {
            return;
          }
          const next = item.next;
          const previous = item.previous;
          if (item === this._head) {
            next.previous = void 0;
            this._head = next;
          } else {
            next.previous = previous;
            previous.next = next;
          }
          item.next = void 0;
          item.previous = this._tail;
          this._tail.next = item;
          this._tail = item;
          this._state++;
        }
      }
      toJSON() {
        const data = [];
        this.forEach((value, key) => {
          data.push([key, value]);
        });
        return data;
      }
      fromJSON(data) {
        this.clear();
        for (const [key, value] of data) {
          this.set(key, value);
        }
      }
    };
    exports2.LinkedMap = LinkedMap;
    var LRUCache = class extends LinkedMap {
      _limit;
      _ratio;
      constructor(limit, ratio = 1) {
        super();
        this._limit = limit;
        this._ratio = Math.min(Math.max(0, ratio), 1);
      }
      get limit() {
        return this._limit;
      }
      set limit(limit) {
        this._limit = limit;
        this.checkTrim();
      }
      get ratio() {
        return this._ratio;
      }
      set ratio(ratio) {
        this._ratio = Math.min(Math.max(0, ratio), 1);
        this.checkTrim();
      }
      get(key, touch = Touch.AsNew) {
        return super.get(key, touch);
      }
      peek(key) {
        return super.get(key, Touch.None);
      }
      set(key, value) {
        super.set(key, value, Touch.Last);
        this.checkTrim();
        return this;
      }
      checkTrim() {
        if (this.size > this._limit) {
          this.trimOld(Math.round(this._limit * this._ratio));
        }
      }
    };
    exports2.LRUCache = LRUCache;
  }
});

// node_modules/vscode-jsonrpc/lib/common/disposable.js
var require_disposable = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/disposable.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.Disposable = void 0;
    var Disposable;
    (function(Disposable2) {
      function create(func) {
        return {
          dispose: func
        };
      }
      Disposable2.create = create;
    })(Disposable || (exports2.Disposable = Disposable = {}));
  }
});

// node_modules/vscode-jsonrpc/lib/common/ral.js
var require_ral = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/ral.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    var _ral;
    function RAL() {
      if (_ral === void 0) {
        throw new Error(`No runtime abstraction layer installed`);
      }
      return _ral;
    }
    (function(RAL2) {
      function install(ral) {
        if (ral === void 0) {
          throw new Error(`No runtime abstraction layer provided`);
        }
        _ral = ral;
      }
      RAL2.install = install;
    })(RAL || (RAL = {}));
    exports2.default = RAL;
  }
});

// node_modules/vscode-jsonrpc/lib/common/events.js
var require_events = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/events.js"(exports2) {
    "use strict";
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.Emitter = exports2.Event = void 0;
    var ral_1 = __importDefault(require_ral());
    var Event;
    (function(Event2) {
      const _disposable = { dispose() {
      } };
      Event2.None = function() {
        return _disposable;
      };
    })(Event || (exports2.Event = Event = {}));
    var CallbackList = class {
      _callbacks;
      _contexts;
      add(callback, context = null, bucket) {
        if (!this._callbacks) {
          this._callbacks = [];
          this._contexts = [];
        }
        this._callbacks.push(callback);
        this._contexts.push(context);
        if (Array.isArray(bucket)) {
          bucket.push({ dispose: () => this.remove(callback, context) });
        }
      }
      remove(callback, context = null) {
        if (!this._callbacks) {
          return;
        }
        let foundCallbackWithDifferentContext = false;
        for (let i = 0, len = this._callbacks.length; i < len; i++) {
          if (this._callbacks[i] === callback) {
            if (this._contexts[i] === context) {
              this._callbacks.splice(i, 1);
              this._contexts.splice(i, 1);
              return;
            } else {
              foundCallbackWithDifferentContext = true;
            }
          }
        }
        if (foundCallbackWithDifferentContext) {
          throw new Error("When adding a listener with a context, you should remove it with the same context");
        }
      }
      invoke(...args) {
        if (!this._callbacks) {
          return [];
        }
        const ret = [], callbacks = this._callbacks.slice(0), contexts = this._contexts.slice(0);
        for (let i = 0, len = callbacks.length; i < len; i++) {
          try {
            ret.push(callbacks[i].apply(contexts[i], args));
          } catch (e) {
            (0, ral_1.default)().console.error(e);
          }
        }
        return ret;
      }
      isEmpty() {
        return !this._callbacks || this._callbacks.length === 0;
      }
      dispose() {
        this._callbacks = void 0;
        this._contexts = void 0;
      }
    };
    var Emitter = class _Emitter {
      _options;
      static _noop = function() {
      };
      _event;
      _callbacks;
      constructor(_options) {
        this._options = _options;
      }
      /**
       * For the public to allow to subscribe
       * to events from this Emitter
       */
      get event() {
        if (!this._event) {
          this._event = (listener, thisArgs, disposables) => {
            if (!this._callbacks) {
              this._callbacks = new CallbackList();
            }
            if (this._options && this._options.onFirstListenerAdd && this._callbacks.isEmpty()) {
              this._options.onFirstListenerAdd(this);
            }
            this._callbacks.add(listener, thisArgs);
            const result = {
              dispose: () => {
                if (!this._callbacks) {
                  return;
                }
                this._callbacks.remove(listener, thisArgs);
                result.dispose = _Emitter._noop;
                if (this._options && this._options.onLastListenerRemove && this._callbacks.isEmpty()) {
                  this._options.onLastListenerRemove(this);
                }
              }
            };
            if (Array.isArray(disposables)) {
              disposables.push(result);
            }
            return result;
          };
        }
        return this._event;
      }
      /**
       * To be kept private to fire an event to
       * subscribers
       */
      fire(event) {
        if (this._callbacks) {
          this._callbacks.invoke.call(this._callbacks, event);
        }
      }
      dispose() {
        if (this._callbacks) {
          this._callbacks.dispose();
          this._callbacks = void 0;
        }
      }
    };
    exports2.Emitter = Emitter;
  }
});

// node_modules/vscode-jsonrpc/lib/common/cancellation.js
var require_cancellation = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/cancellation.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CancellationTokenSource = exports2.CancellationToken = void 0;
    var ral_1 = __importDefault(require_ral());
    var Is2 = __importStar(require_is2());
    var events_1 = require_events();
    var CancellationToken;
    (function(CancellationToken2) {
      CancellationToken2.None = Object.freeze({
        isCancellationRequested: false,
        onCancellationRequested: events_1.Event.None
      });
      CancellationToken2.Cancelled = Object.freeze({
        isCancellationRequested: true,
        onCancellationRequested: events_1.Event.None
      });
      function is(value) {
        const candidate = value;
        return candidate && (candidate === CancellationToken2.None || candidate === CancellationToken2.Cancelled || Is2.boolean(candidate.isCancellationRequested) && !!candidate.onCancellationRequested);
      }
      CancellationToken2.is = is;
    })(CancellationToken || (exports2.CancellationToken = CancellationToken = {}));
    var shortcutEvent = Object.freeze(function(callback, context) {
      const handle = (0, ral_1.default)().timer.setTimeout(callback.bind(context), 0);
      return { dispose() {
        handle.dispose();
      } };
    });
    var MutableToken = class {
      _isCancelled = false;
      _emitter;
      cancel() {
        if (!this._isCancelled) {
          this._isCancelled = true;
          if (this._emitter) {
            this._emitter.fire(void 0);
            this.dispose();
          }
        }
      }
      get isCancellationRequested() {
        return this._isCancelled;
      }
      get onCancellationRequested() {
        if (this._isCancelled) {
          return shortcutEvent;
        }
        if (!this._emitter) {
          this._emitter = new events_1.Emitter();
        }
        return this._emitter.event;
      }
      dispose() {
        if (this._emitter) {
          this._emitter.dispose();
          this._emitter = void 0;
        }
      }
    };
    var CancellationTokenSource = class {
      _token;
      get token() {
        if (!this._token) {
          this._token = new MutableToken();
        }
        return this._token;
      }
      cancel() {
        if (!this._token) {
          this._token = CancellationToken.Cancelled;
        } else {
          this._token.cancel();
        }
      }
      dispose() {
        if (!this._token) {
          this._token = CancellationToken.None;
        } else if (this._token instanceof MutableToken) {
          this._token.dispose();
        }
      }
    };
    exports2.CancellationTokenSource = CancellationTokenSource;
  }
});

// node_modules/vscode-jsonrpc/lib/common/sharedArrayCancellation.js
var require_sharedArrayCancellation = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/sharedArrayCancellation.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SharedArrayReceiverStrategy = exports2.SharedArraySenderStrategy = void 0;
    var cancellation_1 = require_cancellation();
    var CancellationState;
    (function(CancellationState2) {
      CancellationState2.Continue = 0;
      CancellationState2.Cancelled = 1;
    })(CancellationState || (CancellationState = {}));
    var SharedArraySenderStrategy = class {
      buffers;
      constructor() {
        this.buffers = /* @__PURE__ */ new Map();
      }
      enableCancellation(request) {
        if (request.id === null) {
          return;
        }
        const buffer = new SharedArrayBuffer(4);
        const data = new Int32Array(buffer, 0, 1);
        data[0] = CancellationState.Continue;
        this.buffers.set(request.id, buffer);
        request.$cancellationData = buffer;
      }
      async sendCancellation(_conn, id) {
        const buffer = this.buffers.get(id);
        if (buffer === void 0) {
          return;
        }
        const data = new Int32Array(buffer, 0, 1);
        Atomics.store(data, 0, CancellationState.Cancelled);
      }
      cleanup(id) {
        this.buffers.delete(id);
      }
      dispose() {
        this.buffers.clear();
      }
    };
    exports2.SharedArraySenderStrategy = SharedArraySenderStrategy;
    var SharedArrayBufferCancellationToken = class {
      data;
      constructor(buffer) {
        this.data = new Int32Array(buffer, 0, 1);
      }
      get isCancellationRequested() {
        return Atomics.load(this.data, 0) === CancellationState.Cancelled;
      }
      get onCancellationRequested() {
        throw new Error(`Cancellation over SharedArrayBuffer doesn't support cancellation events`);
      }
    };
    var SharedArrayBufferCancellationTokenSource = class {
      token;
      constructor(buffer) {
        this.token = new SharedArrayBufferCancellationToken(buffer);
      }
      cancel() {
      }
      dispose() {
      }
    };
    var SharedArrayReceiverStrategy = class {
      kind = "request";
      createCancellationTokenSource(request) {
        const buffer = request.$cancellationData;
        if (buffer === void 0) {
          return new cancellation_1.CancellationTokenSource();
        }
        return new SharedArrayBufferCancellationTokenSource(buffer);
      }
    };
    exports2.SharedArrayReceiverStrategy = SharedArrayReceiverStrategy;
  }
});

// node_modules/vscode-jsonrpc/lib/common/semaphore.js
var require_semaphore = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/semaphore.js"(exports2) {
    "use strict";
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.Semaphore = void 0;
    var ral_1 = __importDefault(require_ral());
    var Semaphore = class {
      _capacity;
      _active;
      _waiting;
      constructor(capacity = 1) {
        if (capacity <= 0) {
          throw new Error("Capacity must be greater than 0");
        }
        this._capacity = capacity;
        this._active = 0;
        this._waiting = [];
      }
      lock(thunk) {
        return new Promise((resolve, reject) => {
          this._waiting.push({ thunk, resolve, reject });
          this.runNext();
        });
      }
      get active() {
        return this._active;
      }
      runNext() {
        if (this._waiting.length === 0 || this._active === this._capacity) {
          return;
        }
        (0, ral_1.default)().timer.setImmediate(() => this.doRunNext());
      }
      doRunNext() {
        if (this._waiting.length === 0 || this._active === this._capacity) {
          return;
        }
        const next = this._waiting.shift();
        this._active++;
        if (this._active > this._capacity) {
          throw new Error(`Too many thunks active`);
        }
        try {
          const result = next.thunk();
          if (result instanceof Promise) {
            result.then((value) => {
              this._active--;
              next.resolve(value);
              this.runNext();
            }, (err) => {
              this._active--;
              next.reject(err);
              this.runNext();
            });
          } else {
            this._active--;
            next.resolve(result);
            this.runNext();
          }
        } catch (err) {
          this._active--;
          next.reject(err);
          this.runNext();
        }
      }
    };
    exports2.Semaphore = Semaphore;
  }
});

// node_modules/vscode-jsonrpc/lib/common/messageReader.js
var require_messageReader = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/messageReader.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ReadableStreamMessageReader = exports2.AbstractMessageReader = exports2.MessageReader = void 0;
    var ral_1 = __importDefault(require_ral());
    var Is2 = __importStar(require_is2());
    var events_1 = require_events();
    var semaphore_1 = require_semaphore();
    var MessageReader;
    (function(MessageReader2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.func(candidate.listen) && Is2.func(candidate.dispose) && Is2.func(candidate.onError) && Is2.func(candidate.onClose) && Is2.func(candidate.onPartialMessage);
      }
      MessageReader2.is = is;
    })(MessageReader || (exports2.MessageReader = MessageReader = {}));
    var AbstractMessageReader = class {
      errorEmitter;
      closeEmitter;
      partialMessageEmitter;
      constructor() {
        this.errorEmitter = new events_1.Emitter();
        this.closeEmitter = new events_1.Emitter();
        this.partialMessageEmitter = new events_1.Emitter();
      }
      dispose() {
        this.errorEmitter.dispose();
        this.closeEmitter.dispose();
        this.partialMessageEmitter.dispose();
      }
      get onError() {
        return this.errorEmitter.event;
      }
      fireError(error) {
        this.errorEmitter.fire(this.asError(error));
      }
      get onClose() {
        return this.closeEmitter.event;
      }
      fireClose() {
        this.closeEmitter.fire(void 0);
      }
      get onPartialMessage() {
        return this.partialMessageEmitter.event;
      }
      firePartialMessage(info) {
        this.partialMessageEmitter.fire(info);
      }
      asError(error) {
        if (error instanceof Error) {
          return error;
        } else {
          return new Error(`Reader received error. Reason: ${Is2.string(error.message) ? error.message : "unknown"}`);
        }
      }
    };
    exports2.AbstractMessageReader = AbstractMessageReader;
    var ResolvedMessageReaderOptions;
    (function(ResolvedMessageReaderOptions2) {
      function fromOptions(options) {
        let charset;
        let result;
        let contentDecoder;
        const contentDecoders = /* @__PURE__ */ new Map();
        let contentTypeDecoder;
        const contentTypeDecoders = /* @__PURE__ */ new Map();
        if (options === void 0 || typeof options === "string") {
          charset = options ?? "utf-8";
        } else {
          charset = options.charset ?? "utf-8";
          if (options.contentDecoder !== void 0) {
            contentDecoder = options.contentDecoder;
            contentDecoders.set(contentDecoder.name, contentDecoder);
          }
          if (options.contentDecoders !== void 0) {
            for (const decoder of options.contentDecoders) {
              contentDecoders.set(decoder.name, decoder);
            }
          }
          if (options.contentTypeDecoder !== void 0) {
            contentTypeDecoder = options.contentTypeDecoder;
            contentTypeDecoders.set(contentTypeDecoder.name, contentTypeDecoder);
          }
          if (options.contentTypeDecoders !== void 0) {
            for (const decoder of options.contentTypeDecoders) {
              contentTypeDecoders.set(decoder.name, decoder);
            }
          }
        }
        if (contentTypeDecoder === void 0) {
          contentTypeDecoder = (0, ral_1.default)().applicationJson.decoder;
          contentTypeDecoders.set(contentTypeDecoder.name, contentTypeDecoder);
        }
        return { charset, contentDecoder, contentDecoders, contentTypeDecoder, contentTypeDecoders };
      }
      ResolvedMessageReaderOptions2.fromOptions = fromOptions;
    })(ResolvedMessageReaderOptions || (ResolvedMessageReaderOptions = {}));
    var ReadableStreamMessageReader = class extends AbstractMessageReader {
      readable;
      options;
      callback;
      nextMessageLength;
      messageToken;
      buffer;
      partialMessageTimer;
      _partialMessageTimeout;
      readSemaphore;
      constructor(readable, options) {
        super();
        this.readable = readable;
        this.options = ResolvedMessageReaderOptions.fromOptions(options);
        this.buffer = (0, ral_1.default)().messageBuffer.create(this.options.charset);
        this._partialMessageTimeout = 1e4;
        this.nextMessageLength = -1;
        this.messageToken = 0;
        this.readSemaphore = new semaphore_1.Semaphore(1);
      }
      set partialMessageTimeout(timeout) {
        this._partialMessageTimeout = timeout;
      }
      get partialMessageTimeout() {
        return this._partialMessageTimeout;
      }
      listen(callback) {
        this.nextMessageLength = -1;
        this.messageToken = 0;
        this.partialMessageTimer = void 0;
        this.callback = callback;
        const result = this.readable.onData((data) => {
          this.onData(data);
        });
        this.readable.onError((error) => this.fireError(error));
        this.readable.onClose(() => this.fireClose());
        return result;
      }
      onData(data) {
        try {
          this.buffer.append(data);
          while (true) {
            if (this.nextMessageLength === -1) {
              const headers = this.buffer.tryReadHeaders(true);
              if (!headers) {
                return;
              }
              const contentLength = headers.get("content-length");
              if (!contentLength) {
                this.fireError(new Error(`Header must provide a Content-Length property.
${JSON.stringify(Object.fromEntries(headers))}`));
                return;
              }
              const length = parseInt(contentLength);
              if (isNaN(length)) {
                this.fireError(new Error(`Content-Length value must be a number. Got ${contentLength}`));
                return;
              }
              this.nextMessageLength = length;
            }
            const body = this.buffer.tryReadBody(this.nextMessageLength);
            if (body === void 0) {
              this.setPartialMessageTimer();
              return;
            }
            this.clearPartialMessageTimer();
            this.nextMessageLength = -1;
            this.readSemaphore.lock(async () => {
              const bytes = this.options.contentDecoder !== void 0 ? await this.options.contentDecoder.decode(body) : body;
              const message = await this.options.contentTypeDecoder.decode(bytes, this.options);
              this.callback(message);
            }).catch((error) => {
              this.fireError(error);
            });
          }
        } catch (error) {
          this.fireError(error);
        }
      }
      clearPartialMessageTimer() {
        if (this.partialMessageTimer) {
          this.partialMessageTimer.dispose();
          this.partialMessageTimer = void 0;
        }
      }
      setPartialMessageTimer() {
        this.clearPartialMessageTimer();
        if (this._partialMessageTimeout <= 0) {
          return;
        }
        this.partialMessageTimer = (0, ral_1.default)().timer.setTimeout((token, timeout) => {
          this.partialMessageTimer = void 0;
          if (token === this.messageToken) {
            this.firePartialMessage({ messageToken: token, waitingTime: timeout });
            this.setPartialMessageTimer();
          }
        }, this._partialMessageTimeout, this.messageToken, this._partialMessageTimeout);
      }
    };
    exports2.ReadableStreamMessageReader = ReadableStreamMessageReader;
  }
});

// node_modules/vscode-jsonrpc/lib/common/messageWriter.js
var require_messageWriter = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/messageWriter.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WriteableStreamMessageWriter = exports2.AbstractMessageWriter = exports2.MessageWriter = void 0;
    var ral_1 = __importDefault(require_ral());
    var Is2 = __importStar(require_is2());
    var semaphore_1 = require_semaphore();
    var events_1 = require_events();
    var ContentLength = "Content-Length: ";
    var CRLF = "\r\n";
    var MessageWriter;
    (function(MessageWriter2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.func(candidate.dispose) && Is2.func(candidate.onClose) && Is2.func(candidate.onError) && Is2.func(candidate.write);
      }
      MessageWriter2.is = is;
    })(MessageWriter || (exports2.MessageWriter = MessageWriter = {}));
    var AbstractMessageWriter = class {
      errorEmitter;
      closeEmitter;
      constructor() {
        this.errorEmitter = new events_1.Emitter();
        this.closeEmitter = new events_1.Emitter();
      }
      dispose() {
        this.errorEmitter.dispose();
        this.closeEmitter.dispose();
      }
      get onError() {
        return this.errorEmitter.event;
      }
      fireError(error, message, count) {
        this.errorEmitter.fire([this.asError(error), message, count]);
      }
      get onClose() {
        return this.closeEmitter.event;
      }
      fireClose() {
        this.closeEmitter.fire(void 0);
      }
      asError(error) {
        if (error instanceof Error) {
          return error;
        } else {
          return new Error(`Writer received error. Reason: ${Is2.string(error.message) ? error.message : "unknown"}`);
        }
      }
    };
    exports2.AbstractMessageWriter = AbstractMessageWriter;
    var ResolvedMessageWriterOptions;
    (function(ResolvedMessageWriterOptions2) {
      function fromOptions(options) {
        if (options === void 0 || typeof options === "string") {
          return { charset: options ?? "utf-8", contentTypeEncoder: (0, ral_1.default)().applicationJson.encoder };
        } else {
          return { charset: options.charset ?? "utf-8", contentEncoder: options.contentEncoder, contentTypeEncoder: options.contentTypeEncoder ?? (0, ral_1.default)().applicationJson.encoder };
        }
      }
      ResolvedMessageWriterOptions2.fromOptions = fromOptions;
    })(ResolvedMessageWriterOptions || (ResolvedMessageWriterOptions = {}));
    var WriteableStreamMessageWriter = class extends AbstractMessageWriter {
      writable;
      options;
      errorCount;
      writeSemaphore;
      constructor(writable, options) {
        super();
        this.writable = writable;
        this.options = ResolvedMessageWriterOptions.fromOptions(options);
        this.errorCount = 0;
        this.writeSemaphore = new semaphore_1.Semaphore(1);
        this.writable.onError((error) => this.fireError(error));
        this.writable.onClose(() => this.fireClose());
      }
      async write(msg) {
        return this.writeSemaphore.lock(async () => {
          const payload = this.options.contentTypeEncoder.encode(msg, this.options).then((buffer) => {
            if (this.options.contentEncoder !== void 0) {
              return this.options.contentEncoder.encode(buffer);
            } else {
              return buffer;
            }
          });
          return payload.then((buffer) => {
            const headers = [];
            headers.push(ContentLength, buffer.byteLength.toString(), CRLF);
            headers.push(CRLF);
            return this.doWrite(msg, headers, buffer);
          }, (error) => {
            this.fireError(error);
            throw error;
          });
        });
      }
      async doWrite(msg, headers, data) {
        try {
          await this.writable.write(headers.join(""), "ascii");
          return this.writable.write(data);
        } catch (error) {
          this.handleError(error, msg);
          return Promise.reject(error);
        }
      }
      handleError(error, msg) {
        this.errorCount++;
        this.fireError(error, msg, this.errorCount);
      }
      end() {
        this.writable.end();
      }
    };
    exports2.WriteableStreamMessageWriter = WriteableStreamMessageWriter;
  }
});

// node_modules/vscode-jsonrpc/lib/common/messageBuffer.js
var require_messageBuffer = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/messageBuffer.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.AbstractMessageBuffer = void 0;
    var CR = 13;
    var LF = 10;
    var CRLF = "\r\n";
    var AbstractMessageBuffer = class {
      _encoding;
      _chunks;
      _totalLength;
      constructor(encoding = "utf-8") {
        this._encoding = encoding;
        this._chunks = [];
        this._totalLength = 0;
      }
      get encoding() {
        return this._encoding;
      }
      append(chunk) {
        const toAppend = typeof chunk === "string" ? this.fromString(chunk, this._encoding) : chunk;
        this._chunks.push(toAppend);
        this._totalLength += toAppend.byteLength;
      }
      tryReadHeaders(lowerCaseKeys = false) {
        if (this._chunks.length === 0) {
          return void 0;
        }
        let state = 0;
        let chunkIndex = 0;
        let offset = 0;
        let chunkBytesRead = 0;
        row: while (chunkIndex < this._chunks.length) {
          const chunk = this._chunks[chunkIndex];
          offset = 0;
          while (offset < chunk.length) {
            const value = chunk[offset];
            switch (value) {
              case CR:
                switch (state) {
                  case 0:
                    state = 1;
                    break;
                  case 2:
                    state = 3;
                    break;
                  default:
                    state = 0;
                }
                break;
              case LF:
                switch (state) {
                  case 1:
                    state = 2;
                    break;
                  case 3:
                    state = 4;
                    offset++;
                    break row;
                  default:
                    state = 0;
                }
                break;
              default:
                state = 0;
            }
            offset++;
          }
          chunkBytesRead += chunk.byteLength;
          chunkIndex++;
        }
        if (state !== 4) {
          return void 0;
        }
        const buffer = this._read(chunkBytesRead + offset);
        const result = /* @__PURE__ */ new Map();
        const headers = this.toString(buffer, "ascii").split(CRLF);
        if (headers.length < 2) {
          return result;
        }
        for (let i = 0; i < headers.length - 2; i++) {
          const header = headers[i];
          const index = header.indexOf(":");
          if (index === -1) {
            throw new Error(`Message header must separate key and value using ':'
${header}`);
          }
          const key = header.substr(0, index);
          const value = header.substr(index + 1).trim();
          result.set(lowerCaseKeys ? key.toLowerCase() : key, value);
        }
        return result;
      }
      tryReadBody(length) {
        if (this._totalLength < length) {
          return void 0;
        }
        return this._read(length);
      }
      get numberOfBytes() {
        return this._totalLength;
      }
      _read(byteCount) {
        if (byteCount === 0) {
          return this.emptyBuffer();
        }
        if (byteCount > this._totalLength) {
          throw new Error(`Cannot read so many bytes!`);
        }
        if (this._chunks[0].byteLength === byteCount) {
          const chunk = this._chunks[0];
          this._chunks.shift();
          this._totalLength -= byteCount;
          return this.asNative(chunk);
        }
        if (this._chunks[0].byteLength > byteCount) {
          const chunk = this._chunks[0];
          const result2 = this.asNative(chunk, byteCount);
          this._chunks[0] = chunk.slice(byteCount);
          this._totalLength -= byteCount;
          return result2;
        }
        const result = this.allocNative(byteCount);
        let resultOffset = 0;
        const chunkIndex = 0;
        while (byteCount > 0) {
          const chunk = this._chunks[chunkIndex];
          if (chunk.byteLength > byteCount) {
            const chunkPart = chunk.slice(0, byteCount);
            result.set(chunkPart, resultOffset);
            resultOffset += byteCount;
            this._chunks[chunkIndex] = chunk.slice(byteCount);
            this._totalLength -= byteCount;
            byteCount -= byteCount;
          } else {
            result.set(chunk, resultOffset);
            resultOffset += chunk.byteLength;
            this._chunks.shift();
            this._totalLength -= chunk.byteLength;
            byteCount -= chunk.byteLength;
          }
        }
        return result;
      }
    };
    exports2.AbstractMessageBuffer = AbstractMessageBuffer;
  }
});

// node_modules/vscode-jsonrpc/lib/common/connection.js
var require_connection = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/connection.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ConnectionOptions = exports2.MessageStrategy = exports2.CancellationStrategy = exports2.CancellationSenderStrategy = exports2.CancellationReceiverStrategy = exports2.RequestCancellationReceiverStrategy = exports2.IdCancellationReceiverStrategy = exports2.ConnectionStrategy = exports2.ConnectionError = exports2.ConnectionErrors = exports2.LogTraceNotification = exports2.SetTraceNotification = exports2.TraceFormat = exports2.TraceValues = exports2.TraceValue = exports2.Trace = exports2.NullLogger = exports2.ProgressType = exports2.ProgressToken = void 0;
    exports2.createMessageConnection = createMessageConnection;
    var ral_1 = __importDefault(require_ral());
    var Is2 = __importStar(require_is2());
    var messages_1 = require_messages();
    var linkedMap_1 = require_linkedMap();
    var events_1 = require_events();
    var cancellation_1 = require_cancellation();
    var CancelNotification;
    (function(CancelNotification2) {
      CancelNotification2.type = new messages_1.NotificationType("$/cancelRequest");
    })(CancelNotification || (CancelNotification = {}));
    var ProgressToken;
    (function(ProgressToken2) {
      function is(value) {
        return typeof value === "string" || typeof value === "number";
      }
      ProgressToken2.is = is;
    })(ProgressToken || (exports2.ProgressToken = ProgressToken = {}));
    var ProgressNotification;
    (function(ProgressNotification2) {
      ProgressNotification2.type = new messages_1.NotificationType("$/progress");
    })(ProgressNotification || (ProgressNotification = {}));
    var ProgressType = class {
      /**
       * Clients must not use these properties. They are here to ensure correct typing.
       * in TypeScript
       */
      __;
      _pr;
      constructor() {
      }
    };
    exports2.ProgressType = ProgressType;
    var StarRequestHandler;
    (function(StarRequestHandler2) {
      function is(value) {
        return Is2.func(value);
      }
      StarRequestHandler2.is = is;
    })(StarRequestHandler || (StarRequestHandler = {}));
    exports2.NullLogger = Object.freeze({
      error: () => {
      },
      warn: () => {
      },
      info: () => {
      },
      log: () => {
      }
    });
    var Trace;
    (function(Trace2) {
      Trace2[Trace2["Off"] = 0] = "Off";
      Trace2[Trace2["Messages"] = 1] = "Messages";
      Trace2[Trace2["Compact"] = 2] = "Compact";
      Trace2[Trace2["Verbose"] = 3] = "Verbose";
    })(Trace || (exports2.Trace = Trace = {}));
    var TraceValue;
    (function(TraceValue2) {
      TraceValue2.Off = "off";
      TraceValue2.Messages = "messages";
      TraceValue2.Compact = "compact";
      TraceValue2.Verbose = "verbose";
    })(TraceValue || (exports2.TraceValue = TraceValue = {}));
    exports2.TraceValues = TraceValue;
    (function(Trace2) {
      function fromString(value) {
        if (!Is2.string(value)) {
          return Trace2.Off;
        }
        value = value.toLowerCase();
        switch (value) {
          case "off":
            return Trace2.Off;
          case "messages":
            return Trace2.Messages;
          case "compact":
            return Trace2.Compact;
          case "verbose":
            return Trace2.Verbose;
          default:
            return Trace2.Off;
        }
      }
      Trace2.fromString = fromString;
      function toString(value) {
        switch (value) {
          case Trace2.Off:
            return "off";
          case Trace2.Messages:
            return "messages";
          case Trace2.Compact:
            return "compact";
          case Trace2.Verbose:
            return "verbose";
          default:
            return "off";
        }
      }
      Trace2.toString = toString;
    })(Trace || (exports2.Trace = Trace = {}));
    var TraceFormat;
    (function(TraceFormat2) {
      TraceFormat2["Text"] = "text";
      TraceFormat2["JSON"] = "json";
    })(TraceFormat || (exports2.TraceFormat = TraceFormat = {}));
    (function(TraceFormat2) {
      function fromString(value) {
        if (!Is2.string(value)) {
          return TraceFormat2.Text;
        }
        value = value.toLowerCase();
        if (value === "json") {
          return TraceFormat2.JSON;
        } else {
          return TraceFormat2.Text;
        }
      }
      TraceFormat2.fromString = fromString;
    })(TraceFormat || (exports2.TraceFormat = TraceFormat = {}));
    var SetTraceNotification;
    (function(SetTraceNotification2) {
      SetTraceNotification2.type = new messages_1.NotificationType("$/setTrace");
    })(SetTraceNotification || (exports2.SetTraceNotification = SetTraceNotification = {}));
    var LogTraceNotification;
    (function(LogTraceNotification2) {
      LogTraceNotification2.type = new messages_1.NotificationType("$/logTrace");
    })(LogTraceNotification || (exports2.LogTraceNotification = LogTraceNotification = {}));
    var ConnectionErrors;
    (function(ConnectionErrors2) {
      ConnectionErrors2[ConnectionErrors2["Closed"] = 1] = "Closed";
      ConnectionErrors2[ConnectionErrors2["Disposed"] = 2] = "Disposed";
      ConnectionErrors2[ConnectionErrors2["AlreadyListening"] = 3] = "AlreadyListening";
    })(ConnectionErrors || (exports2.ConnectionErrors = ConnectionErrors = {}));
    var ConnectionError = class _ConnectionError extends Error {
      code;
      constructor(code, message) {
        super(message);
        this.code = code;
        Object.setPrototypeOf(this, _ConnectionError.prototype);
      }
    };
    exports2.ConnectionError = ConnectionError;
    var ConnectionStrategy;
    (function(ConnectionStrategy2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.func(candidate.cancelUndispatched);
      }
      ConnectionStrategy2.is = is;
    })(ConnectionStrategy || (exports2.ConnectionStrategy = ConnectionStrategy = {}));
    var IdCancellationReceiverStrategy;
    (function(IdCancellationReceiverStrategy2) {
      function is(value) {
        const candidate = value;
        return candidate && (candidate.kind === void 0 || candidate.kind === "id") && Is2.func(candidate.createCancellationTokenSource) && (candidate.dispose === void 0 || Is2.func(candidate.dispose));
      }
      IdCancellationReceiverStrategy2.is = is;
    })(IdCancellationReceiverStrategy || (exports2.IdCancellationReceiverStrategy = IdCancellationReceiverStrategy = {}));
    var RequestCancellationReceiverStrategy;
    (function(RequestCancellationReceiverStrategy2) {
      function is(value) {
        const candidate = value;
        return candidate && candidate.kind === "request" && Is2.func(candidate.createCancellationTokenSource) && (candidate.dispose === void 0 || Is2.func(candidate.dispose));
      }
      RequestCancellationReceiverStrategy2.is = is;
    })(RequestCancellationReceiverStrategy || (exports2.RequestCancellationReceiverStrategy = RequestCancellationReceiverStrategy = {}));
    var CancellationReceiverStrategy;
    (function(CancellationReceiverStrategy2) {
      CancellationReceiverStrategy2.Message = Object.freeze({
        createCancellationTokenSource(_) {
          return new cancellation_1.CancellationTokenSource();
        }
      });
      function is(value) {
        return IdCancellationReceiverStrategy.is(value) || RequestCancellationReceiverStrategy.is(value);
      }
      CancellationReceiverStrategy2.is = is;
    })(CancellationReceiverStrategy || (exports2.CancellationReceiverStrategy = CancellationReceiverStrategy = {}));
    var CancellationSenderStrategy;
    (function(CancellationSenderStrategy2) {
      CancellationSenderStrategy2.Message = Object.freeze({
        sendCancellation(conn, id) {
          return conn.sendNotification(CancelNotification.type, { id });
        },
        cleanup(_) {
        }
      });
      function is(value) {
        const candidate = value;
        return candidate && Is2.func(candidate.sendCancellation) && Is2.func(candidate.cleanup);
      }
      CancellationSenderStrategy2.is = is;
    })(CancellationSenderStrategy || (exports2.CancellationSenderStrategy = CancellationSenderStrategy = {}));
    var CancellationStrategy;
    (function(CancellationStrategy2) {
      CancellationStrategy2.Message = Object.freeze({
        receiver: CancellationReceiverStrategy.Message,
        sender: CancellationSenderStrategy.Message
      });
      function is(value) {
        const candidate = value;
        return candidate && CancellationReceiverStrategy.is(candidate.receiver) && CancellationSenderStrategy.is(candidate.sender);
      }
      CancellationStrategy2.is = is;
    })(CancellationStrategy || (exports2.CancellationStrategy = CancellationStrategy = {}));
    var MessageStrategy;
    (function(MessageStrategy2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.func(candidate.handleMessage);
      }
      MessageStrategy2.is = is;
    })(MessageStrategy || (exports2.MessageStrategy = MessageStrategy = {}));
    var ConnectionOptions;
    (function(ConnectionOptions2) {
      function is(value) {
        const candidate = value;
        return candidate && (CancellationStrategy.is(candidate.cancellationStrategy) || ConnectionStrategy.is(candidate.connectionStrategy) || MessageStrategy.is(candidate.messageStrategy) || Is2.number(candidate.maxParallelism));
      }
      ConnectionOptions2.is = is;
    })(ConnectionOptions || (exports2.ConnectionOptions = ConnectionOptions = {}));
    var ConnectionState;
    (function(ConnectionState2) {
      ConnectionState2[ConnectionState2["New"] = 1] = "New";
      ConnectionState2[ConnectionState2["Listening"] = 2] = "Listening";
      ConnectionState2[ConnectionState2["Closed"] = 3] = "Closed";
      ConnectionState2[ConnectionState2["Disposed"] = 4] = "Disposed";
    })(ConnectionState || (ConnectionState = {}));
    function createMessageConnection(messageReader, messageWriter, _logger, options) {
      const logger = _logger !== void 0 ? _logger : exports2.NullLogger;
      let sequenceNumber = 0;
      let notificationSequenceNumber = 0;
      let unknownResponseSequenceNumber = 0;
      const version = "2.0";
      const maxParallelism = options?.maxParallelism ?? -1;
      let inFlight = 0;
      let starRequestHandler = void 0;
      const requestHandlers = /* @__PURE__ */ new Map();
      let starNotificationHandler = void 0;
      const notificationHandlers = /* @__PURE__ */ new Map();
      const progressHandlers = /* @__PURE__ */ new Map();
      let timer;
      let messageQueue = new linkedMap_1.LinkedMap();
      let responsePromises = /* @__PURE__ */ new Map();
      let knownCanceledRequests = /* @__PURE__ */ new Set();
      let requestTokens = /* @__PURE__ */ new Map();
      let trace = Trace.Off;
      let traceFormat = TraceFormat.Text;
      let tracer;
      let state = ConnectionState.New;
      const errorEmitter = new events_1.Emitter();
      const closeEmitter = new events_1.Emitter();
      const unhandledNotificationEmitter = new events_1.Emitter();
      const unhandledProgressEmitter = new events_1.Emitter();
      const disposeEmitter = new events_1.Emitter();
      const cancellationStrategy = options && options.cancellationStrategy ? options.cancellationStrategy : CancellationStrategy.Message;
      function cancelUndispatched(_message) {
        return void 0;
      }
      function isListening() {
        return state === ConnectionState.Listening;
      }
      function isClosed() {
        return state === ConnectionState.Closed;
      }
      function isDisposed() {
        return state === ConnectionState.Disposed;
      }
      function closeHandler() {
        if (state === ConnectionState.New || state === ConnectionState.Listening) {
          state = ConnectionState.Closed;
          closeEmitter.fire(void 0);
        }
      }
      function readErrorHandler(error) {
        errorEmitter.fire([error, void 0, void 0]);
      }
      function writeErrorHandler(data) {
        errorEmitter.fire(data);
      }
      messageReader.onClose(closeHandler);
      messageReader.onError(readErrorHandler);
      messageWriter.onClose(closeHandler);
      messageWriter.onError(writeErrorHandler);
      function createRequestQueueKey(id) {
        if (id === null) {
          throw new Error(`Can't send requests with id null since the response can't be correlated.`);
        }
        return "req-" + id.toString();
      }
      function createResponseQueueKey(id) {
        if (id === null) {
          return "res-unknown-" + (++unknownResponseSequenceNumber).toString();
        } else {
          return "res-" + id.toString();
        }
      }
      function createNotificationQueueKey() {
        return "not-" + (++notificationSequenceNumber).toString();
      }
      function addMessageToQueue(queue, message) {
        if (messages_1.Message.isRequest(message)) {
          queue.set(createRequestQueueKey(message.id), message);
        } else if (messages_1.Message.isResponse(message)) {
          if (maxParallelism === -1) {
            queue.set(createResponseQueueKey(message.id), message);
          } else {
            handleResponse(message);
          }
        } else {
          queue.set(createNotificationQueueKey(), message);
        }
      }
      function triggerMessageQueue() {
        if (timer || messageQueue.size === 0) {
          return;
        }
        if (maxParallelism !== -1 && inFlight >= maxParallelism) {
          return;
        }
        timer = (0, ral_1.default)().timer.setImmediate(async () => {
          timer = void 0;
          if (messageQueue.size === 0) {
            return;
          }
          if (maxParallelism !== -1 && inFlight >= maxParallelism) {
            return;
          }
          const message = messageQueue.shift();
          let result;
          try {
            inFlight++;
            const messageStrategy = options?.messageStrategy;
            if (MessageStrategy.is(messageStrategy)) {
              result = messageStrategy.handleMessage(message, handleMessage);
            } else {
              result = handleMessage(message);
            }
          } catch (error) {
            logger.error(`Processing message queue failed: ${error.toString()}`);
          } finally {
            if (result instanceof Promise) {
              result.then(() => {
                inFlight--;
                triggerMessageQueue();
              }).catch((error) => {
                logger.error(`Processing message queue failed: ${error.toString()}`);
              });
            } else {
              inFlight--;
            }
            triggerMessageQueue();
          }
        });
      }
      async function handleMessage(message) {
        if (messages_1.Message.isRequest(message)) {
          return handleRequest(message);
        } else if (messages_1.Message.isNotification(message)) {
          return handleNotification(message);
        } else if (messages_1.Message.isResponse(message)) {
          return handleResponse(message);
        } else {
          return handleInvalidMessage(message);
        }
      }
      const callback = (message) => {
        try {
          if (messages_1.Message.isNotification(message) && message.method === CancelNotification.type.method) {
            const cancelId = message.params.id;
            const key = createRequestQueueKey(cancelId);
            const toCancel = messageQueue.get(key);
            if (messages_1.Message.isRequest(toCancel)) {
              const strategy = options?.connectionStrategy;
              const response = strategy && strategy.cancelUndispatched ? strategy.cancelUndispatched(toCancel, cancelUndispatched) : cancelUndispatched(toCancel);
              if (response && (response.error !== void 0 || response.result !== void 0)) {
                messageQueue.delete(key);
                requestTokens.delete(cancelId);
                response.id = toCancel.id;
                traceSendingResponse(response, message.method, Date.now());
                messageWriter.write(response).catch(() => logger.error(`Sending response for canceled message failed.`));
                return;
              }
            }
            const cancellationToken = requestTokens.get(cancelId);
            if (cancellationToken !== void 0) {
              cancellationToken.cancel();
              traceReceivedNotification(message);
              return;
            } else {
              knownCanceledRequests.add(cancelId);
            }
          }
          addMessageToQueue(messageQueue, message);
        } finally {
          triggerMessageQueue();
        }
      };
      async function handleRequest(requestMessage) {
        if (isDisposed()) {
          return Promise.resolve();
        }
        function reply(resultOrError, method, startTime2) {
          const message = {
            jsonrpc: version,
            id: requestMessage.id
          };
          if (resultOrError instanceof messages_1.ResponseError) {
            message.error = resultOrError.toJson();
          } else {
            message.result = resultOrError === void 0 ? null : resultOrError;
          }
          traceSendingResponse(message, method, startTime2);
          return messageWriter.write(message);
        }
        function replyError(error, method, startTime2) {
          const message = {
            jsonrpc: version,
            id: requestMessage.id,
            error: error.toJson()
          };
          traceSendingResponse(message, method, startTime2);
          return messageWriter.write(message);
        }
        traceReceivedRequest(requestMessage);
        const element = requestHandlers.get(requestMessage.method);
        let type;
        let requestHandler;
        if (element) {
          type = element.type;
          requestHandler = element.handler;
        }
        const startTime = Date.now();
        if (requestHandler || starRequestHandler) {
          const tokenKey = requestMessage.id ?? String(Date.now());
          const cancellationSource = IdCancellationReceiverStrategy.is(cancellationStrategy.receiver) ? cancellationStrategy.receiver.createCancellationTokenSource(tokenKey) : cancellationStrategy.receiver.createCancellationTokenSource(requestMessage);
          if (requestMessage.id !== null && knownCanceledRequests.has(requestMessage.id)) {
            cancellationSource.cancel();
          }
          if (requestMessage.id !== null) {
            requestTokens.set(tokenKey, cancellationSource);
          }
          try {
            let handlerResult;
            if (requestHandler) {
              if (requestMessage.params === void 0) {
                if (type !== void 0 && type.numberOfParams !== 0) {
                  return replyError(new messages_1.ResponseError(messages_1.ErrorCodes.InvalidParams, `Request ${requestMessage.method} defines ${type.numberOfParams} params but received none.`), requestMessage.method, startTime);
                }
                handlerResult = requestHandler(cancellationSource.token);
              } else if (Array.isArray(requestMessage.params)) {
                if (type !== void 0 && type.parameterStructures === messages_1.ParameterStructures.byName) {
                  return replyError(new messages_1.ResponseError(messages_1.ErrorCodes.InvalidParams, `Request ${requestMessage.method} defines parameters by name but received parameters by position`), requestMessage.method, startTime);
                }
                handlerResult = requestHandler(...requestMessage.params, cancellationSource.token);
              } else {
                if (type !== void 0 && type.parameterStructures === messages_1.ParameterStructures.byPosition) {
                  return replyError(new messages_1.ResponseError(messages_1.ErrorCodes.InvalidParams, `Request ${requestMessage.method} defines parameters by position but received parameters by name`), requestMessage.method, startTime);
                }
                handlerResult = requestHandler(requestMessage.params, cancellationSource.token);
              }
            } else if (starRequestHandler) {
              handlerResult = starRequestHandler(requestMessage.method, requestMessage.params, cancellationSource.token);
            }
            const resultOrError = await handlerResult;
            await reply(resultOrError, requestMessage.method, startTime);
          } catch (error) {
            if (error instanceof messages_1.ResponseError) {
              await reply(error, requestMessage.method, startTime);
            } else if (error && Is2.string(error.message)) {
              await replyError(new messages_1.ResponseError(messages_1.ErrorCodes.InternalError, `Request ${requestMessage.method} failed with message: ${error.message}`), requestMessage.method, startTime);
            } else {
              await replyError(new messages_1.ResponseError(messages_1.ErrorCodes.InternalError, `Request ${requestMessage.method} failed unexpectedly without providing any details.`), requestMessage.method, startTime);
            }
          } finally {
            requestTokens.delete(tokenKey);
          }
        } else {
          await replyError(new messages_1.ResponseError(messages_1.ErrorCodes.MethodNotFound, `Unhandled method ${requestMessage.method}`), requestMessage.method, startTime);
        }
      }
      function handleResponse(responseMessage) {
        if (isDisposed()) {
          return;
        }
        if (responseMessage.id === null) {
          if (responseMessage.error) {
            logger.error(`Received response message without id: Error is:
${JSON.stringify(responseMessage.error, void 0, 4)}`);
          } else {
            logger.error(`Received response message without id. No further error information provided.`);
          }
        } else {
          const key = responseMessage.id;
          const responsePromise = responsePromises.get(key);
          traceReceivedResponse(responseMessage, responsePromise);
          if (responsePromise !== void 0) {
            responsePromises.delete(key);
            try {
              if (responseMessage.error) {
                const error = responseMessage.error;
                responsePromise.reject(new messages_1.ResponseError(error.code, error.message, error.data));
              } else if (responseMessage.result !== void 0) {
                responsePromise.resolve(responseMessage.result);
              } else {
                throw new Error("Should never happen.");
              }
            } catch (error) {
              if (error.message) {
                logger.error(`Response handler '${responsePromise.method}' failed with message: ${error.message}`);
              } else {
                logger.error(`Response handler '${responsePromise.method}' failed unexpectedly.`);
              }
            }
          }
        }
      }
      async function handleNotification(message) {
        if (isDisposed()) {
          return;
        }
        let type = void 0;
        let notificationHandler;
        if (message.method === CancelNotification.type.method) {
          const cancelId = message.params.id;
          knownCanceledRequests.delete(cancelId);
          traceReceivedNotification(message);
          return;
        } else {
          const element = notificationHandlers.get(message.method);
          if (element) {
            notificationHandler = element.handler;
            type = element.type;
          }
        }
        if (notificationHandler || starNotificationHandler) {
          try {
            traceReceivedNotification(message);
            if (notificationHandler) {
              if (message.params === void 0) {
                if (type !== void 0) {
                  if (type.numberOfParams !== 0 && type.parameterStructures !== messages_1.ParameterStructures.byName) {
                    logger.error(`Notification ${message.method} defines ${type.numberOfParams} params but received none.`);
                  }
                }
                await notificationHandler();
              } else if (Array.isArray(message.params)) {
                const params = message.params;
                if (message.method === ProgressNotification.type.method && params.length === 2 && ProgressToken.is(params[0])) {
                  await notificationHandler({ token: params[0], value: params[1] });
                } else {
                  if (type !== void 0) {
                    if (type.parameterStructures === messages_1.ParameterStructures.byName) {
                      logger.error(`Notification ${message.method} defines parameters by name but received parameters by position`);
                    }
                    if (type.numberOfParams !== message.params.length) {
                      logger.error(`Notification ${message.method} defines ${type.numberOfParams} params but received ${params.length} arguments`);
                    }
                  }
                  await notificationHandler(...params);
                }
              } else {
                if (type !== void 0 && type.parameterStructures === messages_1.ParameterStructures.byPosition) {
                  logger.error(`Notification ${message.method} defines parameters by position but received parameters by name`);
                }
                await notificationHandler(message.params);
              }
            } else if (starNotificationHandler) {
              await starNotificationHandler(message.method, message.params);
            }
          } catch (error) {
            if (error.message) {
              logger.error(`Notification handler '${message.method}' failed with message: ${error.message}`);
            } else {
              logger.error(`Notification handler '${message.method}' failed unexpectedly.`);
            }
          }
        } else {
          unhandledNotificationEmitter.fire(message);
        }
      }
      function handleInvalidMessage(message) {
        if (!message) {
          logger.error("Received empty message.");
          return;
        }
        logger.error(`Received message which is neither a response nor a notification message:
${JSON.stringify(message, null, 4)}`);
        const responseMessage = message;
        if (Is2.string(responseMessage.id) || Is2.number(responseMessage.id)) {
          const key = responseMessage.id;
          const responseHandler = responsePromises.get(key);
          if (responseHandler) {
            responseHandler.reject(new Error("The received response has neither a result nor an error property."));
          }
        }
      }
      function stringifyTrace(params) {
        if (params === void 0 || params === null) {
          return void 0;
        }
        switch (trace) {
          case Trace.Verbose:
            return JSON.stringify(params, null, 4);
          case Trace.Compact:
            return JSON.stringify(params);
          default:
            return void 0;
        }
      }
      function traceSendingRequest(message) {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if ((trace === Trace.Verbose || trace === Trace.Compact) && message.params) {
            data = `Params: ${stringifyTrace(message.params)}`;
          }
          tracer.log(`Sending request '${message.method} - (${message.id})'.`, data);
        } else {
          logLSPMessage("send-request", message);
        }
      }
      function traceSendingNotification(message) {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if (trace === Trace.Verbose || trace === Trace.Compact) {
            if (message.params) {
              data = `Params: ${stringifyTrace(message.params)}`;
            } else {
              data = "No parameters provided.";
            }
          }
          tracer.log(`Sending notification '${message.method}'.`, data);
        } else {
          logLSPMessage("send-notification", message);
        }
      }
      function traceSendingResponse(message, method, startTime) {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if (trace === Trace.Verbose || trace === Trace.Compact) {
            if (message.error && message.error.data) {
              data = `Error data: ${stringifyTrace(message.error.data)}`;
            } else {
              if (message.result) {
                data = `Result: ${stringifyTrace(message.result)}`;
              } else if (message.error === void 0) {
                data = "No result returned.";
              }
            }
          }
          tracer.log(`Sending response '${method} - (${message.id})'. Processing request took ${Date.now() - startTime}ms`, data);
        } else {
          logLSPMessage("send-response", message);
        }
      }
      function traceReceivedRequest(message) {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if ((trace === Trace.Verbose || trace === Trace.Compact) && message.params) {
            data = `Params: ${stringifyTrace(message.params)}`;
          }
          tracer.log(`Received request '${message.method} - (${message.id})'.`, data);
        } else {
          logLSPMessage("receive-request", message);
        }
      }
      function traceReceivedNotification(message) {
        if (trace === Trace.Off || !tracer || message.method === LogTraceNotification.type.method) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if (trace === Trace.Verbose || trace === Trace.Compact) {
            if (message.params) {
              data = `Params: ${stringifyTrace(message.params)}`;
            } else {
              data = "No parameters provided.";
            }
          }
          tracer.log(`Received notification '${message.method}'.`, data);
        } else {
          logLSPMessage("receive-notification", message);
        }
      }
      function traceReceivedResponse(message, responsePromise) {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        if (traceFormat === TraceFormat.Text) {
          let data = void 0;
          if (trace === Trace.Verbose || trace === Trace.Compact) {
            if (message.error && message.error.data) {
              data = `Error data: ${stringifyTrace(message.error.data)}`;
            } else {
              if (message.result) {
                data = `Result: ${stringifyTrace(message.result)}`;
              } else if (message.error === void 0) {
                data = "No result returned.";
              }
            }
          }
          if (responsePromise) {
            const error = message.error ? ` Request failed: ${message.error.message} (${message.error.code}).` : "";
            tracer.log(`Received response '${responsePromise.method} - (${message.id})' in ${Date.now() - responsePromise.timerStart}ms.${error}`, data);
          } else {
            tracer.log(`Received response ${message.id} without active response promise.`, data);
          }
        } else {
          logLSPMessage("receive-response", message);
        }
      }
      function logLSPMessage(type, message) {
        if (!tracer || trace === Trace.Off) {
          return;
        }
        const lspMessage = {
          isLSPMessage: true,
          type,
          message,
          timestamp: Date.now()
        };
        tracer.log(lspMessage);
      }
      function throwIfClosedOrDisposed() {
        if (isClosed()) {
          throw new ConnectionError(ConnectionErrors.Closed, "Connection is closed.");
        }
        if (isDisposed()) {
          throw new ConnectionError(ConnectionErrors.Disposed, "Connection is disposed.");
        }
      }
      function throwIfListening() {
        if (isListening()) {
          throw new ConnectionError(ConnectionErrors.AlreadyListening, "Connection is already listening");
        }
      }
      function throwIfNotListening() {
        if (!isListening()) {
          throw new Error("Call listen() first.");
        }
      }
      function undefinedToNull(param) {
        if (param === void 0) {
          return null;
        } else {
          return param;
        }
      }
      function nullToUndefined(param) {
        if (param === null) {
          return void 0;
        } else {
          return param;
        }
      }
      function isNamedParam(param) {
        return param !== void 0 && param !== null && !Array.isArray(param) && typeof param === "object";
      }
      function computeSingleParam(parameterStructures, param) {
        switch (parameterStructures) {
          case messages_1.ParameterStructures.auto:
            if (isNamedParam(param)) {
              return nullToUndefined(param);
            } else {
              return [undefinedToNull(param)];
            }
          case messages_1.ParameterStructures.byName:
            if (!isNamedParam(param)) {
              throw new Error(`Received parameters by name but param is not an object literal.`);
            }
            return nullToUndefined(param);
          case messages_1.ParameterStructures.byPosition:
            return [undefinedToNull(param)];
          default:
            throw new Error(`Unknown parameter structure ${parameterStructures.toString()}`);
        }
      }
      function computeMessageParams(type, params) {
        let result;
        const numberOfParams = type.numberOfParams;
        switch (numberOfParams) {
          case 0:
            result = void 0;
            break;
          case 1:
            result = computeSingleParam(type.parameterStructures, params[0]);
            break;
          default:
            result = [];
            for (let i = 0; i < params.length && i < numberOfParams; i++) {
              result.push(undefinedToNull(params[i]));
            }
            if (params.length < numberOfParams) {
              for (let i = params.length; i < numberOfParams; i++) {
                result.push(null);
              }
            }
            break;
        }
        return result;
      }
      const connection = {
        sendNotification: (type, ...args) => {
          throwIfClosedOrDisposed();
          let method;
          let messageParams;
          if (Is2.string(type)) {
            method = type;
            const first = args[0];
            let paramStart = 0;
            let parameterStructures = messages_1.ParameterStructures.auto;
            if (messages_1.ParameterStructures.is(first)) {
              paramStart = 1;
              parameterStructures = first;
            }
            const paramEnd = args.length;
            const numberOfParams = paramEnd - paramStart;
            switch (numberOfParams) {
              case 0:
                messageParams = void 0;
                break;
              case 1:
                messageParams = computeSingleParam(parameterStructures, args[paramStart]);
                break;
              default:
                if (parameterStructures === messages_1.ParameterStructures.byName) {
                  throw new Error(`Received ${numberOfParams} parameters for 'by Name' notification parameter structure.`);
                }
                messageParams = args.slice(paramStart, paramEnd).map((value) => undefinedToNull(value));
                break;
            }
          } else {
            const params = args;
            method = type.method;
            messageParams = computeMessageParams(type, params);
          }
          const notificationMessage = {
            jsonrpc: version,
            method,
            params: messageParams
          };
          traceSendingNotification(notificationMessage);
          return messageWriter.write(notificationMessage).catch((error) => {
            logger.error(`Sending notification failed.`);
            throw error;
          });
        },
        onNotification: (type, handler) => {
          throwIfClosedOrDisposed();
          let method;
          if (Is2.func(type)) {
            starNotificationHandler = type;
          } else if (handler) {
            if (Is2.string(type)) {
              method = type;
              notificationHandlers.set(type, { type: void 0, handler });
            } else {
              method = type.method;
              notificationHandlers.set(type.method, { type, handler });
            }
          }
          return {
            dispose: () => {
              if (method !== void 0) {
                if (notificationHandlers.get(method)?.handler === handler) {
                  notificationHandlers.delete(method);
                }
              } else if (starNotificationHandler === type) {
                starNotificationHandler = void 0;
              }
            }
          };
        },
        onProgress: (_type, token, handler) => {
          if (progressHandlers.has(token)) {
            throw new Error(`Progress handler for token ${token} already registered`);
          }
          progressHandlers.set(token, handler);
          return {
            dispose: () => {
              if (progressHandlers.get(token) === handler) {
                progressHandlers.delete(token);
              }
            }
          };
        },
        sendProgress: (_type, token, value) => {
          return connection.sendNotification(ProgressNotification.type, { token, value });
        },
        onUnhandledProgress: unhandledProgressEmitter.event,
        sendRequest: (type, ...args) => {
          throwIfClosedOrDisposed();
          throwIfNotListening();
          function sendCancellation(connection2, id2) {
            const p = cancellationStrategy.sender.sendCancellation(connection2, id2);
            if (p === void 0) {
              logger.log(`Received no promise from cancellation strategy when cancelling id ${id2}`);
            } else {
              p.catch(() => {
                logger.log(`Sending cancellation messages for id ${id2} failed.`);
              });
            }
          }
          let method;
          let messageParams;
          let token = void 0;
          if (Is2.string(type)) {
            method = type;
            const first = args[0];
            const last = args[args.length - 1];
            let paramStart = 0;
            let parameterStructures = messages_1.ParameterStructures.auto;
            if (messages_1.ParameterStructures.is(first)) {
              paramStart = 1;
              parameterStructures = first;
            }
            let paramEnd = args.length;
            if (cancellation_1.CancellationToken.is(last)) {
              paramEnd = paramEnd - 1;
              token = last;
            }
            const numberOfParams = paramEnd - paramStart;
            switch (numberOfParams) {
              case 0:
                messageParams = void 0;
                break;
              case 1:
                messageParams = computeSingleParam(parameterStructures, args[paramStart]);
                break;
              default:
                if (parameterStructures === messages_1.ParameterStructures.byName) {
                  throw new Error(`Received ${numberOfParams} parameters for 'by Name' request parameter structure.`);
                }
                messageParams = args.slice(paramStart, paramEnd).map((value) => undefinedToNull(value));
                break;
            }
          } else {
            const params = args;
            method = type.method;
            messageParams = computeMessageParams(type, params);
            const numberOfParams = type.numberOfParams;
            token = cancellation_1.CancellationToken.is(params[numberOfParams]) ? params[numberOfParams] : void 0;
          }
          const id = sequenceNumber++;
          let disposable;
          let tokenWasCancelled = false;
          if (token !== void 0) {
            if (token.isCancellationRequested) {
              tokenWasCancelled = true;
            } else {
              disposable = token.onCancellationRequested(() => {
                sendCancellation(connection, id);
              });
            }
          }
          const requestMessage = {
            jsonrpc: version,
            id,
            method,
            params: messageParams
          };
          traceSendingRequest(requestMessage);
          if (typeof cancellationStrategy.sender.enableCancellation === "function") {
            cancellationStrategy.sender.enableCancellation(requestMessage);
          }
          return new Promise(async (resolve, reject) => {
            const resolveWithCleanup = (r) => {
              resolve(r);
              cancellationStrategy.sender.cleanup(id);
              disposable?.dispose();
            };
            const rejectWithCleanup = (r) => {
              reject(r);
              cancellationStrategy.sender.cleanup(id);
              disposable?.dispose();
            };
            const responsePromise = { method, timerStart: Date.now(), resolve: resolveWithCleanup, reject: rejectWithCleanup };
            try {
              responsePromises.set(id, responsePromise);
              await messageWriter.write(requestMessage);
              if (tokenWasCancelled) {
                sendCancellation(connection, id);
              }
            } catch (error) {
              responsePromises.delete(id);
              responsePromise.reject(new messages_1.ResponseError(messages_1.ErrorCodes.MessageWriteError, error.message ? error.message : "Unknown reason"));
              logger.error(`Sending request failed.`);
              throw error;
            }
          });
        },
        onRequest: (type, handler) => {
          throwIfClosedOrDisposed();
          let method = null;
          if (StarRequestHandler.is(type)) {
            method = void 0;
            starRequestHandler = type;
          } else if (Is2.string(type)) {
            method = null;
            if (handler !== void 0) {
              method = type;
              requestHandlers.set(type, { handler, type: void 0 });
            }
          } else {
            if (handler !== void 0) {
              method = type.method;
              requestHandlers.set(type.method, { type, handler });
            }
          }
          return {
            dispose: () => {
              if (method === null) {
                return;
              }
              if (method !== void 0) {
                if (requestHandlers.get(method)?.handler === handler) {
                  requestHandlers.delete(method);
                }
              } else if (starRequestHandler === type) {
                starRequestHandler = void 0;
              }
            }
          };
        },
        hasPendingResponse: () => {
          return responsePromises.size > 0;
        },
        trace: async (_value, _tracer, sendNotificationOrTraceOptions) => {
          let _sendNotification = false;
          let _traceFormat = TraceFormat.Text;
          if (sendNotificationOrTraceOptions !== void 0) {
            if (Is2.boolean(sendNotificationOrTraceOptions)) {
              _sendNotification = sendNotificationOrTraceOptions;
            } else {
              _sendNotification = sendNotificationOrTraceOptions.sendNotification || false;
              _traceFormat = sendNotificationOrTraceOptions.traceFormat || TraceFormat.Text;
            }
          }
          trace = _value;
          traceFormat = _traceFormat;
          if (trace === Trace.Off) {
            tracer = void 0;
          } else {
            tracer = _tracer;
          }
          if (_sendNotification && !isClosed() && !isDisposed()) {
            await connection.sendNotification(SetTraceNotification.type, { value: Trace.toString(_value) });
          }
        },
        onError: errorEmitter.event,
        onClose: closeEmitter.event,
        onUnhandledNotification: unhandledNotificationEmitter.event,
        onDispose: disposeEmitter.event,
        end: () => {
          messageWriter.end();
        },
        dispose: () => {
          if (isDisposed()) {
            return;
          }
          state = ConnectionState.Disposed;
          disposeEmitter.fire(void 0);
          const error = new messages_1.ResponseError(messages_1.ErrorCodes.PendingResponseRejected, "Pending response rejected since connection got disposed");
          for (const promise of responsePromises.values()) {
            promise.reject(error);
          }
          responsePromises = /* @__PURE__ */ new Map();
          requestTokens = /* @__PURE__ */ new Map();
          knownCanceledRequests = /* @__PURE__ */ new Set();
          messageQueue = new linkedMap_1.LinkedMap();
          if (Is2.func(messageWriter.dispose)) {
            messageWriter.dispose();
          }
          if (Is2.func(messageReader.dispose)) {
            messageReader.dispose();
          }
        },
        listen: () => {
          throwIfClosedOrDisposed();
          throwIfListening();
          state = ConnectionState.Listening;
          messageReader.listen(callback);
        },
        inspect: () => {
          (0, ral_1.default)().console.log("inspect");
        }
      };
      connection.onNotification(LogTraceNotification.type, (params) => {
        if (trace === Trace.Off || !tracer) {
          return;
        }
        const verbose = trace === Trace.Verbose || trace === Trace.Compact;
        tracer.log(params.message, verbose ? params.verbose : void 0);
      });
      connection.onNotification(ProgressNotification.type, async (params) => {
        const handler = progressHandlers.get(params.token);
        if (handler) {
          await handler(params.value);
        } else {
          unhandledProgressEmitter.fire(params);
        }
      });
      return connection;
    }
  }
});

// node_modules/vscode-jsonrpc/lib/common/api.js
var require_api = __commonJS({
  "node_modules/vscode-jsonrpc/lib/common/api.js"(exports2) {
    "use strict";
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ProgressType = exports2.ProgressToken = exports2.createMessageConnection = exports2.NullLogger = exports2.ConnectionOptions = exports2.ConnectionStrategy = exports2.AbstractMessageBuffer = exports2.WriteableStreamMessageWriter = exports2.AbstractMessageWriter = exports2.MessageWriter = exports2.ReadableStreamMessageReader = exports2.AbstractMessageReader = exports2.MessageReader = exports2.SharedArrayReceiverStrategy = exports2.SharedArraySenderStrategy = exports2.CancellationToken = exports2.CancellationTokenSource = exports2.Emitter = exports2.Event = exports2.Disposable = exports2.LRUCache = exports2.Touch = exports2.LinkedMap = exports2.ParameterStructures = exports2.NotificationType9 = exports2.NotificationType8 = exports2.NotificationType7 = exports2.NotificationType6 = exports2.NotificationType5 = exports2.NotificationType4 = exports2.NotificationType3 = exports2.NotificationType2 = exports2.NotificationType1 = exports2.NotificationType0 = exports2.NotificationType = exports2.ErrorCodes = exports2.ResponseError = exports2.RequestType9 = exports2.RequestType8 = exports2.RequestType7 = exports2.RequestType6 = exports2.RequestType5 = exports2.RequestType4 = exports2.RequestType3 = exports2.RequestType2 = exports2.RequestType1 = exports2.RequestType0 = exports2.RequestType = exports2.Message = exports2.RAL = void 0;
    exports2.MessageStrategy = exports2.CancellationStrategy = exports2.CancellationSenderStrategy = exports2.RequestCancellationReceiverStrategy = exports2.IdCancellationReceiverStrategy = exports2.CancellationReceiverStrategy = exports2.ConnectionError = exports2.ConnectionErrors = exports2.LogTraceNotification = exports2.SetTraceNotification = exports2.TraceFormat = exports2.TraceValues = exports2.TraceValue = exports2.Trace = void 0;
    var messages_1 = require_messages();
    Object.defineProperty(exports2, "Message", { enumerable: true, get: function() {
      return messages_1.Message;
    } });
    Object.defineProperty(exports2, "RequestType", { enumerable: true, get: function() {
      return messages_1.RequestType;
    } });
    Object.defineProperty(exports2, "RequestType0", { enumerable: true, get: function() {
      return messages_1.RequestType0;
    } });
    Object.defineProperty(exports2, "RequestType1", { enumerable: true, get: function() {
      return messages_1.RequestType1;
    } });
    Object.defineProperty(exports2, "RequestType2", { enumerable: true, get: function() {
      return messages_1.RequestType2;
    } });
    Object.defineProperty(exports2, "RequestType3", { enumerable: true, get: function() {
      return messages_1.RequestType3;
    } });
    Object.defineProperty(exports2, "RequestType4", { enumerable: true, get: function() {
      return messages_1.RequestType4;
    } });
    Object.defineProperty(exports2, "RequestType5", { enumerable: true, get: function() {
      return messages_1.RequestType5;
    } });
    Object.defineProperty(exports2, "RequestType6", { enumerable: true, get: function() {
      return messages_1.RequestType6;
    } });
    Object.defineProperty(exports2, "RequestType7", { enumerable: true, get: function() {
      return messages_1.RequestType7;
    } });
    Object.defineProperty(exports2, "RequestType8", { enumerable: true, get: function() {
      return messages_1.RequestType8;
    } });
    Object.defineProperty(exports2, "RequestType9", { enumerable: true, get: function() {
      return messages_1.RequestType9;
    } });
    Object.defineProperty(exports2, "ResponseError", { enumerable: true, get: function() {
      return messages_1.ResponseError;
    } });
    Object.defineProperty(exports2, "ErrorCodes", { enumerable: true, get: function() {
      return messages_1.ErrorCodes;
    } });
    Object.defineProperty(exports2, "NotificationType", { enumerable: true, get: function() {
      return messages_1.NotificationType;
    } });
    Object.defineProperty(exports2, "NotificationType0", { enumerable: true, get: function() {
      return messages_1.NotificationType0;
    } });
    Object.defineProperty(exports2, "NotificationType1", { enumerable: true, get: function() {
      return messages_1.NotificationType1;
    } });
    Object.defineProperty(exports2, "NotificationType2", { enumerable: true, get: function() {
      return messages_1.NotificationType2;
    } });
    Object.defineProperty(exports2, "NotificationType3", { enumerable: true, get: function() {
      return messages_1.NotificationType3;
    } });
    Object.defineProperty(exports2, "NotificationType4", { enumerable: true, get: function() {
      return messages_1.NotificationType4;
    } });
    Object.defineProperty(exports2, "NotificationType5", { enumerable: true, get: function() {
      return messages_1.NotificationType5;
    } });
    Object.defineProperty(exports2, "NotificationType6", { enumerable: true, get: function() {
      return messages_1.NotificationType6;
    } });
    Object.defineProperty(exports2, "NotificationType7", { enumerable: true, get: function() {
      return messages_1.NotificationType7;
    } });
    Object.defineProperty(exports2, "NotificationType8", { enumerable: true, get: function() {
      return messages_1.NotificationType8;
    } });
    Object.defineProperty(exports2, "NotificationType9", { enumerable: true, get: function() {
      return messages_1.NotificationType9;
    } });
    Object.defineProperty(exports2, "ParameterStructures", { enumerable: true, get: function() {
      return messages_1.ParameterStructures;
    } });
    var linkedMap_1 = require_linkedMap();
    Object.defineProperty(exports2, "LinkedMap", { enumerable: true, get: function() {
      return linkedMap_1.LinkedMap;
    } });
    Object.defineProperty(exports2, "LRUCache", { enumerable: true, get: function() {
      return linkedMap_1.LRUCache;
    } });
    Object.defineProperty(exports2, "Touch", { enumerable: true, get: function() {
      return linkedMap_1.Touch;
    } });
    var disposable_1 = require_disposable();
    Object.defineProperty(exports2, "Disposable", { enumerable: true, get: function() {
      return disposable_1.Disposable;
    } });
    var events_1 = require_events();
    Object.defineProperty(exports2, "Event", { enumerable: true, get: function() {
      return events_1.Event;
    } });
    Object.defineProperty(exports2, "Emitter", { enumerable: true, get: function() {
      return events_1.Emitter;
    } });
    var cancellation_1 = require_cancellation();
    Object.defineProperty(exports2, "CancellationTokenSource", { enumerable: true, get: function() {
      return cancellation_1.CancellationTokenSource;
    } });
    Object.defineProperty(exports2, "CancellationToken", { enumerable: true, get: function() {
      return cancellation_1.CancellationToken;
    } });
    var sharedArrayCancellation_1 = require_sharedArrayCancellation();
    Object.defineProperty(exports2, "SharedArraySenderStrategy", { enumerable: true, get: function() {
      return sharedArrayCancellation_1.SharedArraySenderStrategy;
    } });
    Object.defineProperty(exports2, "SharedArrayReceiverStrategy", { enumerable: true, get: function() {
      return sharedArrayCancellation_1.SharedArrayReceiverStrategy;
    } });
    var messageReader_1 = require_messageReader();
    Object.defineProperty(exports2, "MessageReader", { enumerable: true, get: function() {
      return messageReader_1.MessageReader;
    } });
    Object.defineProperty(exports2, "AbstractMessageReader", { enumerable: true, get: function() {
      return messageReader_1.AbstractMessageReader;
    } });
    Object.defineProperty(exports2, "ReadableStreamMessageReader", { enumerable: true, get: function() {
      return messageReader_1.ReadableStreamMessageReader;
    } });
    var messageWriter_1 = require_messageWriter();
    Object.defineProperty(exports2, "MessageWriter", { enumerable: true, get: function() {
      return messageWriter_1.MessageWriter;
    } });
    Object.defineProperty(exports2, "AbstractMessageWriter", { enumerable: true, get: function() {
      return messageWriter_1.AbstractMessageWriter;
    } });
    Object.defineProperty(exports2, "WriteableStreamMessageWriter", { enumerable: true, get: function() {
      return messageWriter_1.WriteableStreamMessageWriter;
    } });
    var messageBuffer_1 = require_messageBuffer();
    Object.defineProperty(exports2, "AbstractMessageBuffer", { enumerable: true, get: function() {
      return messageBuffer_1.AbstractMessageBuffer;
    } });
    var connection_1 = require_connection();
    Object.defineProperty(exports2, "ConnectionStrategy", { enumerable: true, get: function() {
      return connection_1.ConnectionStrategy;
    } });
    Object.defineProperty(exports2, "ConnectionOptions", { enumerable: true, get: function() {
      return connection_1.ConnectionOptions;
    } });
    Object.defineProperty(exports2, "NullLogger", { enumerable: true, get: function() {
      return connection_1.NullLogger;
    } });
    Object.defineProperty(exports2, "createMessageConnection", { enumerable: true, get: function() {
      return connection_1.createMessageConnection;
    } });
    Object.defineProperty(exports2, "ProgressToken", { enumerable: true, get: function() {
      return connection_1.ProgressToken;
    } });
    Object.defineProperty(exports2, "ProgressType", { enumerable: true, get: function() {
      return connection_1.ProgressType;
    } });
    Object.defineProperty(exports2, "Trace", { enumerable: true, get: function() {
      return connection_1.Trace;
    } });
    Object.defineProperty(exports2, "TraceValue", { enumerable: true, get: function() {
      return connection_1.TraceValue;
    } });
    Object.defineProperty(exports2, "TraceFormat", { enumerable: true, get: function() {
      return connection_1.TraceFormat;
    } });
    Object.defineProperty(exports2, "SetTraceNotification", { enumerable: true, get: function() {
      return connection_1.SetTraceNotification;
    } });
    Object.defineProperty(exports2, "LogTraceNotification", { enumerable: true, get: function() {
      return connection_1.LogTraceNotification;
    } });
    Object.defineProperty(exports2, "ConnectionErrors", { enumerable: true, get: function() {
      return connection_1.ConnectionErrors;
    } });
    Object.defineProperty(exports2, "ConnectionError", { enumerable: true, get: function() {
      return connection_1.ConnectionError;
    } });
    Object.defineProperty(exports2, "CancellationReceiverStrategy", { enumerable: true, get: function() {
      return connection_1.CancellationReceiverStrategy;
    } });
    Object.defineProperty(exports2, "IdCancellationReceiverStrategy", { enumerable: true, get: function() {
      return connection_1.IdCancellationReceiverStrategy;
    } });
    Object.defineProperty(exports2, "RequestCancellationReceiverStrategy", { enumerable: true, get: function() {
      return connection_1.RequestCancellationReceiverStrategy;
    } });
    Object.defineProperty(exports2, "CancellationSenderStrategy", { enumerable: true, get: function() {
      return connection_1.CancellationSenderStrategy;
    } });
    Object.defineProperty(exports2, "CancellationStrategy", { enumerable: true, get: function() {
      return connection_1.CancellationStrategy;
    } });
    Object.defineProperty(exports2, "MessageStrategy", { enumerable: true, get: function() {
      return connection_1.MessageStrategy;
    } });
    Object.defineProperty(exports2, "TraceValues", { enumerable: true, get: function() {
      return connection_1.TraceValues;
    } });
    var ral_1 = __importDefault(require_ral());
    exports2.RAL = ral_1.default;
  }
});

// node_modules/vscode-languageserver-types/lib/esm/main.js
var main_exports = {};
__export(main_exports, {
  AnnotatedTextEdit: () => AnnotatedTextEdit,
  ApplyKind: () => ApplyKind,
  ChangeAnnotation: () => ChangeAnnotation,
  ChangeAnnotationIdentifier: () => ChangeAnnotationIdentifier,
  CodeAction: () => CodeAction,
  CodeActionContext: () => CodeActionContext,
  CodeActionKind: () => CodeActionKind,
  CodeActionTag: () => CodeActionTag,
  CodeActionTriggerKind: () => CodeActionTriggerKind,
  CodeDescription: () => CodeDescription,
  CodeLens: () => CodeLens,
  Color: () => Color,
  ColorInformation: () => ColorInformation,
  ColorPresentation: () => ColorPresentation,
  Command: () => Command,
  CompletionItem: () => CompletionItem,
  CompletionItemKind: () => CompletionItemKind,
  CompletionItemLabelDetails: () => CompletionItemLabelDetails,
  CompletionItemTag: () => CompletionItemTag,
  CompletionList: () => CompletionList,
  CreateFile: () => CreateFile,
  DeleteFile: () => DeleteFile,
  Diagnostic: () => Diagnostic,
  DiagnosticRelatedInformation: () => DiagnosticRelatedInformation,
  DiagnosticSeverity: () => DiagnosticSeverity,
  DiagnosticTag: () => DiagnosticTag,
  DocumentHighlight: () => DocumentHighlight,
  DocumentHighlightKind: () => DocumentHighlightKind,
  DocumentLink: () => DocumentLink,
  DocumentSymbol: () => DocumentSymbol,
  DocumentUri: () => DocumentUri,
  EOL: () => EOL,
  FoldingRange: () => FoldingRange,
  FoldingRangeKind: () => FoldingRangeKind,
  FormattingOptions: () => FormattingOptions,
  Hover: () => Hover,
  InlayHint: () => InlayHint,
  InlayHintKind: () => InlayHintKind,
  InlayHintLabelPart: () => InlayHintLabelPart,
  InlineCompletionContext: () => InlineCompletionContext,
  InlineCompletionItem: () => InlineCompletionItem,
  InlineCompletionList: () => InlineCompletionList,
  InlineCompletionTriggerKind: () => InlineCompletionTriggerKind,
  InlineValueContext: () => InlineValueContext,
  InlineValueEvaluatableExpression: () => InlineValueEvaluatableExpression,
  InlineValueText: () => InlineValueText,
  InlineValueVariableLookup: () => InlineValueVariableLookup,
  InsertReplaceEdit: () => InsertReplaceEdit,
  InsertTextFormat: () => InsertTextFormat,
  InsertTextMode: () => InsertTextMode,
  LanguageKind: () => LanguageKind,
  Location: () => Location,
  LocationLink: () => LocationLink,
  MarkedString: () => MarkedString,
  MarkupContent: () => MarkupContent,
  MarkupKind: () => MarkupKind,
  OptionalVersionedTextDocumentIdentifier: () => OptionalVersionedTextDocumentIdentifier,
  ParameterInformation: () => ParameterInformation,
  Position: () => Position,
  Range: () => Range,
  RenameFile: () => RenameFile,
  SelectedCompletionInfo: () => SelectedCompletionInfo,
  SelectionRange: () => SelectionRange,
  SemanticTokenModifiers: () => SemanticTokenModifiers,
  SemanticTokenTypes: () => SemanticTokenTypes,
  SemanticTokens: () => SemanticTokens,
  SignatureInformation: () => SignatureInformation,
  SnippetTextEdit: () => SnippetTextEdit,
  StringValue: () => StringValue,
  SymbolInformation: () => SymbolInformation,
  SymbolKind: () => SymbolKind,
  SymbolTag: () => SymbolTag,
  TextDocument: () => TextDocument,
  TextDocumentEdit: () => TextDocumentEdit,
  TextDocumentIdentifier: () => TextDocumentIdentifier,
  TextDocumentItem: () => TextDocumentItem,
  TextEdit: () => TextEdit,
  URI: () => URI,
  VersionedTextDocumentIdentifier: () => VersionedTextDocumentIdentifier,
  WorkspaceChange: () => WorkspaceChange,
  WorkspaceEdit: () => WorkspaceEdit,
  WorkspaceFolder: () => WorkspaceFolder,
  WorkspaceSymbol: () => WorkspaceSymbol,
  integer: () => integer,
  uinteger: () => uinteger
});
var DocumentUri, URI, integer, uinteger, Position, Range, Location, LocationLink, Color, ColorInformation, ColorPresentation, FoldingRangeKind, FoldingRange, DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag, CodeDescription, Diagnostic, Command, TextEdit, ChangeAnnotation, ChangeAnnotationIdentifier, AnnotatedTextEdit, TextDocumentEdit, CreateFile, RenameFile, DeleteFile, WorkspaceEdit, TextEditChangeImpl, SnippetTextEdit, ChangeAnnotations, WorkspaceChange, TextDocumentIdentifier, VersionedTextDocumentIdentifier, OptionalVersionedTextDocumentIdentifier, LanguageKind, TextDocumentItem, MarkupKind, MarkupContent, CompletionItemKind, InsertTextFormat, CompletionItemTag, InsertReplaceEdit, InsertTextMode, ApplyKind, CompletionItemLabelDetails, CompletionItem, CompletionList, MarkedString, Hover, ParameterInformation, SignatureInformation, DocumentHighlightKind, DocumentHighlight, SymbolKind, SymbolTag, SymbolInformation, WorkspaceSymbol, DocumentSymbol, CodeActionKind, CodeActionTriggerKind, CodeActionContext, CodeActionTag, CodeAction, CodeLens, FormattingOptions, DocumentLink, SelectionRange, SemanticTokenTypes, SemanticTokenModifiers, SemanticTokens, InlineValueText, InlineValueVariableLookup, InlineValueEvaluatableExpression, InlineValueContext, InlayHintKind, InlayHintLabelPart, InlayHint, StringValue, InlineCompletionItem, InlineCompletionList, InlineCompletionTriggerKind, SelectedCompletionInfo, InlineCompletionContext, WorkspaceFolder, EOL, TextDocument, FullTextDocument, Is;
var init_main = __esm({
  "node_modules/vscode-languageserver-types/lib/esm/main.js"() {
    "use strict";
    (function(DocumentUri2) {
      function is(value) {
        return typeof value === "string";
      }
      DocumentUri2.is = is;
    })(DocumentUri || (DocumentUri = {}));
    (function(URI2) {
      function is(value) {
        return typeof value === "string";
      }
      URI2.is = is;
    })(URI || (URI = {}));
    (function(integer2) {
      integer2.MIN_VALUE = -2147483648;
      integer2.MAX_VALUE = 2147483647;
      function is(value) {
        return typeof value === "number" && integer2.MIN_VALUE <= value && value <= integer2.MAX_VALUE;
      }
      integer2.is = is;
    })(integer || (integer = {}));
    (function(uinteger2) {
      uinteger2.MIN_VALUE = 0;
      uinteger2.MAX_VALUE = 2147483647;
      function is(value) {
        return typeof value === "number" && uinteger2.MIN_VALUE <= value && value <= uinteger2.MAX_VALUE;
      }
      uinteger2.is = is;
    })(uinteger || (uinteger = {}));
    (function(Position2) {
      function create(line, character) {
        if (line === Number.MAX_VALUE) {
          line = uinteger.MAX_VALUE;
        }
        if (character === Number.MAX_VALUE) {
          character = uinteger.MAX_VALUE;
        }
        return { line, character };
      }
      Position2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.uinteger(candidate.line) && Is.uinteger(candidate.character);
      }
      Position2.is = is;
    })(Position || (Position = {}));
    (function(Range2) {
      function create(one, two, three, four) {
        if (Is.uinteger(one) && Is.uinteger(two) && Is.uinteger(three) && Is.uinteger(four)) {
          return { start: Position.create(one, two), end: Position.create(three, four) };
        } else if (Position.is(one) && Position.is(two)) {
          return { start: one, end: two };
        } else {
          throw new Error(`Range#create called with invalid arguments[${one}, ${two}, ${three}, ${four}]`);
        }
      }
      Range2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Position.is(candidate.start) && Position.is(candidate.end);
      }
      Range2.is = is;
    })(Range || (Range = {}));
    (function(Location2) {
      function create(uri, range) {
        return { uri, range };
      }
      Location2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Range.is(candidate.range) && (Is.string(candidate.uri) || Is.undefined(candidate.uri));
      }
      Location2.is = is;
    })(Location || (Location = {}));
    (function(LocationLink2) {
      function create(targetUri, targetRange, targetSelectionRange, originSelectionRange) {
        return { targetUri, targetRange, targetSelectionRange, originSelectionRange };
      }
      LocationLink2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Range.is(candidate.targetRange) && Is.string(candidate.targetUri) && Range.is(candidate.targetSelectionRange) && (Range.is(candidate.originSelectionRange) || Is.undefined(candidate.originSelectionRange));
      }
      LocationLink2.is = is;
    })(LocationLink || (LocationLink = {}));
    (function(Color2) {
      function create(red, green, blue, alpha) {
        return {
          red,
          green,
          blue,
          alpha
        };
      }
      Color2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.numberRange(candidate.red, 0, 1) && Is.numberRange(candidate.green, 0, 1) && Is.numberRange(candidate.blue, 0, 1) && Is.numberRange(candidate.alpha, 0, 1);
      }
      Color2.is = is;
    })(Color || (Color = {}));
    (function(ColorInformation2) {
      function create(range, color) {
        return {
          range,
          color
        };
      }
      ColorInformation2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Range.is(candidate.range) && Color.is(candidate.color);
      }
      ColorInformation2.is = is;
    })(ColorInformation || (ColorInformation = {}));
    (function(ColorPresentation2) {
      function create(label, textEdit, additionalTextEdits) {
        return {
          label,
          textEdit,
          additionalTextEdits
        };
      }
      ColorPresentation2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.string(candidate.label) && (Is.undefined(candidate.textEdit) || TextEdit.is(candidate)) && (Is.undefined(candidate.additionalTextEdits) || Is.typedArray(candidate.additionalTextEdits, TextEdit.is));
      }
      ColorPresentation2.is = is;
    })(ColorPresentation || (ColorPresentation = {}));
    (function(FoldingRangeKind2) {
      FoldingRangeKind2.Comment = "comment";
      FoldingRangeKind2.Imports = "imports";
      FoldingRangeKind2.Region = "region";
    })(FoldingRangeKind || (FoldingRangeKind = {}));
    (function(FoldingRange2) {
      function create(startLine, endLine, startCharacter, endCharacter, kind, collapsedText) {
        const result = {
          startLine,
          endLine
        };
        if (Is.defined(startCharacter)) {
          result.startCharacter = startCharacter;
        }
        if (Is.defined(endCharacter)) {
          result.endCharacter = endCharacter;
        }
        if (Is.defined(kind)) {
          result.kind = kind;
        }
        if (Is.defined(collapsedText)) {
          result.collapsedText = collapsedText;
        }
        return result;
      }
      FoldingRange2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.uinteger(candidate.startLine) && Is.uinteger(candidate.startLine) && (Is.undefined(candidate.startCharacter) || Is.uinteger(candidate.startCharacter)) && (Is.undefined(candidate.endCharacter) || Is.uinteger(candidate.endCharacter)) && (Is.undefined(candidate.kind) || Is.string(candidate.kind));
      }
      FoldingRange2.is = is;
    })(FoldingRange || (FoldingRange = {}));
    (function(DiagnosticRelatedInformation2) {
      function create(location, message) {
        return {
          location,
          message
        };
      }
      DiagnosticRelatedInformation2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Location.is(candidate.location) && Is.string(candidate.message);
      }
      DiagnosticRelatedInformation2.is = is;
    })(DiagnosticRelatedInformation || (DiagnosticRelatedInformation = {}));
    (function(DiagnosticSeverity2) {
      DiagnosticSeverity2.Error = 1;
      DiagnosticSeverity2.Warning = 2;
      DiagnosticSeverity2.Information = 3;
      DiagnosticSeverity2.Hint = 4;
    })(DiagnosticSeverity || (DiagnosticSeverity = {}));
    (function(DiagnosticTag2) {
      DiagnosticTag2.Unnecessary = 1;
      DiagnosticTag2.Deprecated = 2;
    })(DiagnosticTag || (DiagnosticTag = {}));
    (function(CodeDescription2) {
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.string(candidate.href);
      }
      CodeDescription2.is = is;
    })(CodeDescription || (CodeDescription = {}));
    (function(Diagnostic2) {
      function create(range, message, severity, code, source, relatedInformation) {
        const result = { range, message };
        if (Is.defined(severity)) {
          result.severity = severity;
        }
        if (Is.defined(code)) {
          result.code = code;
        }
        if (Is.defined(source)) {
          result.source = source;
        }
        if (Is.defined(relatedInformation)) {
          result.relatedInformation = relatedInformation;
        }
        return result;
      }
      Diagnostic2.create = create;
      function is(value) {
        var _a;
        const candidate = value;
        return Is.defined(candidate) && Range.is(candidate.range) && (Is.string(candidate.message) || MarkupContent.is(candidate.message)) && (Is.number(candidate.severity) || Is.undefined(candidate.severity)) && (Is.integer(candidate.code) || Is.string(candidate.code) || Is.undefined(candidate.code)) && (Is.undefined(candidate.codeDescription) || Is.string((_a = candidate.codeDescription) === null || _a === void 0 ? void 0 : _a.href)) && (Is.string(candidate.source) || Is.undefined(candidate.source)) && (Is.undefined(candidate.relatedInformation) || Is.typedArray(candidate.relatedInformation, DiagnosticRelatedInformation.is));
      }
      Diagnostic2.is = is;
      function is3_17(value) {
        return Is.string(value.message);
      }
      Diagnostic2.is3_17 = is3_17;
      function getMessageString(diagnostic) {
        if (Is.string(diagnostic.message)) {
          return diagnostic.message;
        } else if (MarkupContent.is(diagnostic.message)) {
          return diagnostic.message.value;
        } else {
          throw new Error(`Unknown message type ${typeof diagnostic.message}`);
        }
      }
      Diagnostic2.getMessageString = getMessageString;
    })(Diagnostic || (Diagnostic = {}));
    (function(Command2) {
      function create(title, command, ...args) {
        const result = { title, command };
        if (Is.defined(args) && args.length > 0) {
          result.arguments = args;
        }
        return result;
      }
      Command2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.title) && (candidate.tooltip === void 0 || Is.string(candidate.tooltip)) && Is.string(candidate.command);
      }
      Command2.is = is;
    })(Command || (Command = {}));
    (function(TextEdit2) {
      function replace(range, newText) {
        return { range, newText };
      }
      TextEdit2.replace = replace;
      function insert(position, newText) {
        return { range: { start: position, end: position }, newText };
      }
      TextEdit2.insert = insert;
      function del(range) {
        return { range, newText: "" };
      }
      TextEdit2.del = del;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.string(candidate.newText) && Range.is(candidate.range);
      }
      TextEdit2.is = is;
    })(TextEdit || (TextEdit = {}));
    (function(ChangeAnnotation2) {
      function create(label, needsConfirmation, description) {
        const result = { label };
        if (needsConfirmation !== void 0) {
          result.needsConfirmation = needsConfirmation;
        }
        if (description !== void 0) {
          result.description = description;
        }
        return result;
      }
      ChangeAnnotation2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Is.string(candidate.label) && (Is.boolean(candidate.needsConfirmation) || candidate.needsConfirmation === void 0) && (Is.string(candidate.description) || candidate.description === void 0);
      }
      ChangeAnnotation2.is = is;
    })(ChangeAnnotation || (ChangeAnnotation = {}));
    (function(ChangeAnnotationIdentifier2) {
      function is(value) {
        const candidate = value;
        return Is.string(candidate);
      }
      ChangeAnnotationIdentifier2.is = is;
    })(ChangeAnnotationIdentifier || (ChangeAnnotationIdentifier = {}));
    (function(AnnotatedTextEdit2) {
      function replace(range, newText, annotation) {
        return { range, newText, annotationId: annotation };
      }
      AnnotatedTextEdit2.replace = replace;
      function insert(position, newText, annotation) {
        return { range: { start: position, end: position }, newText, annotationId: annotation };
      }
      AnnotatedTextEdit2.insert = insert;
      function del(range, annotation) {
        return { range, newText: "", annotationId: annotation };
      }
      AnnotatedTextEdit2.del = del;
      function is(value) {
        const candidate = value;
        return TextEdit.is(candidate) && (ChangeAnnotation.is(candidate.annotationId) || ChangeAnnotationIdentifier.is(candidate.annotationId));
      }
      AnnotatedTextEdit2.is = is;
    })(AnnotatedTextEdit || (AnnotatedTextEdit = {}));
    (function(TextDocumentEdit2) {
      function create(textDocument, edits) {
        return { textDocument, edits };
      }
      TextDocumentEdit2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && OptionalVersionedTextDocumentIdentifier.is(candidate.textDocument) && Array.isArray(candidate.edits);
      }
      TextDocumentEdit2.is = is;
    })(TextDocumentEdit || (TextDocumentEdit = {}));
    (function(CreateFile2) {
      function create(uri, options, annotation) {
        const result = {
          kind: "create",
          uri
        };
        if (options !== void 0 && (options.overwrite !== void 0 || options.ignoreIfExists !== void 0)) {
          result.options = options;
        }
        if (annotation !== void 0) {
          result.annotationId = annotation;
        }
        return result;
      }
      CreateFile2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && candidate.kind === "create" && Is.string(candidate.uri) && (candidate.options === void 0 || (candidate.options.overwrite === void 0 || Is.boolean(candidate.options.overwrite)) && (candidate.options.ignoreIfExists === void 0 || Is.boolean(candidate.options.ignoreIfExists))) && (candidate.annotationId === void 0 || ChangeAnnotationIdentifier.is(candidate.annotationId));
      }
      CreateFile2.is = is;
    })(CreateFile || (CreateFile = {}));
    (function(RenameFile2) {
      function create(oldUri, newUri, options, annotation) {
        const result = {
          kind: "rename",
          oldUri,
          newUri
        };
        if (options !== void 0 && (options.overwrite !== void 0 || options.ignoreIfExists !== void 0)) {
          result.options = options;
        }
        if (annotation !== void 0) {
          result.annotationId = annotation;
        }
        return result;
      }
      RenameFile2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && candidate.kind === "rename" && Is.string(candidate.oldUri) && Is.string(candidate.newUri) && (candidate.options === void 0 || (candidate.options.overwrite === void 0 || Is.boolean(candidate.options.overwrite)) && (candidate.options.ignoreIfExists === void 0 || Is.boolean(candidate.options.ignoreIfExists))) && (candidate.annotationId === void 0 || ChangeAnnotationIdentifier.is(candidate.annotationId));
      }
      RenameFile2.is = is;
    })(RenameFile || (RenameFile = {}));
    (function(DeleteFile2) {
      function create(uri, options, annotation) {
        const result = {
          kind: "delete",
          uri
        };
        if (options !== void 0 && (options.recursive !== void 0 || options.ignoreIfNotExists !== void 0)) {
          result.options = options;
        }
        if (annotation !== void 0) {
          result.annotationId = annotation;
        }
        return result;
      }
      DeleteFile2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && candidate.kind === "delete" && Is.string(candidate.uri) && (candidate.options === void 0 || (candidate.options.recursive === void 0 || Is.boolean(candidate.options.recursive)) && (candidate.options.ignoreIfNotExists === void 0 || Is.boolean(candidate.options.ignoreIfNotExists))) && (candidate.annotationId === void 0 || ChangeAnnotationIdentifier.is(candidate.annotationId));
      }
      DeleteFile2.is = is;
    })(DeleteFile || (DeleteFile = {}));
    (function(WorkspaceEdit2) {
      function is(value) {
        const candidate = value;
        return candidate && (candidate.changes !== void 0 || candidate.documentChanges !== void 0) && (candidate.documentChanges === void 0 || candidate.documentChanges.every((change) => {
          if (Is.string(change.kind)) {
            return CreateFile.is(change) || RenameFile.is(change) || DeleteFile.is(change);
          } else {
            return TextDocumentEdit.is(change);
          }
        }));
      }
      WorkspaceEdit2.is = is;
    })(WorkspaceEdit || (WorkspaceEdit = {}));
    TextEditChangeImpl = class {
      constructor(edits, changeAnnotations) {
        this.edits = edits;
        this.changeAnnotations = changeAnnotations;
      }
      insert(position, newText, annotation) {
        let edit;
        let id;
        if (annotation === void 0) {
          edit = TextEdit.insert(position, newText);
        } else if (ChangeAnnotationIdentifier.is(annotation)) {
          id = annotation;
          edit = AnnotatedTextEdit.insert(position, newText, annotation);
        } else {
          this.assertChangeAnnotations(this.changeAnnotations);
          id = this.changeAnnotations.manage(annotation);
          edit = AnnotatedTextEdit.insert(position, newText, id);
        }
        this.edits.push(edit);
        if (id !== void 0) {
          return id;
        }
      }
      replace(range, newText, annotation) {
        let edit;
        let id;
        if (annotation === void 0) {
          edit = TextEdit.replace(range, newText);
        } else if (ChangeAnnotationIdentifier.is(annotation)) {
          id = annotation;
          edit = AnnotatedTextEdit.replace(range, newText, annotation);
        } else {
          this.assertChangeAnnotations(this.changeAnnotations);
          id = this.changeAnnotations.manage(annotation);
          edit = AnnotatedTextEdit.replace(range, newText, id);
        }
        this.edits.push(edit);
        if (id !== void 0) {
          return id;
        }
      }
      delete(range, annotation) {
        let edit;
        let id;
        if (annotation === void 0) {
          edit = TextEdit.del(range);
        } else if (ChangeAnnotationIdentifier.is(annotation)) {
          id = annotation;
          edit = AnnotatedTextEdit.del(range, annotation);
        } else {
          this.assertChangeAnnotations(this.changeAnnotations);
          id = this.changeAnnotations.manage(annotation);
          edit = AnnotatedTextEdit.del(range, id);
        }
        this.edits.push(edit);
        if (id !== void 0) {
          return id;
        }
      }
      add(edit) {
        this.edits.push(edit);
      }
      all() {
        return this.edits;
      }
      clear() {
        this.edits.splice(0, this.edits.length);
      }
      assertChangeAnnotations(value) {
        if (value === void 0) {
          throw new Error(`Text edit change is not configured to manage change annotations.`);
        }
      }
    };
    (function(SnippetTextEdit2) {
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Range.is(candidate.range) && StringValue.isSnippet(candidate.snippet) && (candidate.annotationId === void 0 || (ChangeAnnotation.is(candidate.annotationId) || ChangeAnnotationIdentifier.is(candidate.annotationId)));
      }
      SnippetTextEdit2.is = is;
    })(SnippetTextEdit || (SnippetTextEdit = {}));
    ChangeAnnotations = class {
      constructor(annotations) {
        this._annotations = annotations === void 0 ? /* @__PURE__ */ Object.create(null) : annotations;
        this._counter = 0;
        this._size = 0;
      }
      all() {
        return this._annotations;
      }
      get size() {
        return this._size;
      }
      manage(idOrAnnotation, annotation) {
        let id;
        if (ChangeAnnotationIdentifier.is(idOrAnnotation)) {
          id = idOrAnnotation;
        } else {
          id = this.nextId();
          annotation = idOrAnnotation;
        }
        if (this._annotations[id] !== void 0) {
          throw new Error(`Id ${id} is already in use.`);
        }
        if (annotation === void 0) {
          throw new Error(`No annotation provided for id ${id}`);
        }
        this._annotations[id] = annotation;
        this._size++;
        return id;
      }
      nextId() {
        this._counter++;
        return this._counter.toString();
      }
    };
    WorkspaceChange = class {
      constructor(workspaceEdit) {
        this._textEditChanges = /* @__PURE__ */ Object.create(null);
        if (workspaceEdit !== void 0) {
          this._workspaceEdit = workspaceEdit;
          if (workspaceEdit.documentChanges) {
            this._changeAnnotations = new ChangeAnnotations(workspaceEdit.changeAnnotations);
            workspaceEdit.changeAnnotations = this._changeAnnotations.all();
            workspaceEdit.documentChanges.forEach((change) => {
              if (TextDocumentEdit.is(change)) {
                const textEditChange = new TextEditChangeImpl(change.edits, this._changeAnnotations);
                this._textEditChanges[change.textDocument.uri] = textEditChange;
              }
            });
          } else if (workspaceEdit.changes) {
            Object.keys(workspaceEdit.changes).forEach((key) => {
              const textEditChange = new TextEditChangeImpl(workspaceEdit.changes[key]);
              this._textEditChanges[key] = textEditChange;
            });
          }
        } else {
          this._workspaceEdit = {};
        }
      }
      /**
       * Returns the underlying {@link WorkspaceEdit} literal
       * use to be returned from a workspace edit operation like rename.
       */
      get edit() {
        this.initDocumentChanges();
        if (this._changeAnnotations !== void 0) {
          if (this._changeAnnotations.size === 0) {
            this._workspaceEdit.changeAnnotations = void 0;
          } else {
            this._workspaceEdit.changeAnnotations = this._changeAnnotations.all();
          }
        }
        return this._workspaceEdit;
      }
      getTextEditChange(key) {
        if (OptionalVersionedTextDocumentIdentifier.is(key)) {
          this.initDocumentChanges();
          if (this._workspaceEdit.documentChanges === void 0) {
            throw new Error("Workspace edit is not configured for document changes.");
          }
          const textDocument = { uri: key.uri, version: key.version };
          let result = this._textEditChanges[textDocument.uri];
          if (!result) {
            const edits = [];
            const textDocumentEdit = {
              textDocument,
              edits
            };
            this._workspaceEdit.documentChanges.push(textDocumentEdit);
            result = new TextEditChangeImpl(edits, this._changeAnnotations);
            this._textEditChanges[textDocument.uri] = result;
          }
          return result;
        } else {
          this.initChanges();
          if (this._workspaceEdit.changes === void 0) {
            throw new Error("Workspace edit is not configured for normal text edit changes.");
          }
          let result = this._textEditChanges[key];
          if (!result) {
            const edits = [];
            this._workspaceEdit.changes[key] = edits;
            result = new TextEditChangeImpl(edits);
            this._textEditChanges[key] = result;
          }
          return result;
        }
      }
      initDocumentChanges() {
        if (this._workspaceEdit.documentChanges === void 0 && this._workspaceEdit.changes === void 0) {
          this._changeAnnotations = new ChangeAnnotations();
          this._workspaceEdit.documentChanges = [];
          this._workspaceEdit.changeAnnotations = this._changeAnnotations.all();
        }
      }
      initChanges() {
        if (this._workspaceEdit.documentChanges === void 0 && this._workspaceEdit.changes === void 0) {
          this._workspaceEdit.changes = /* @__PURE__ */ Object.create(null);
        }
      }
      createFile(uri, optionsOrAnnotation, options) {
        this.initDocumentChanges();
        if (this._workspaceEdit.documentChanges === void 0) {
          throw new Error("Workspace edit is not configured for document changes.");
        }
        let annotation;
        if (ChangeAnnotation.is(optionsOrAnnotation) || ChangeAnnotationIdentifier.is(optionsOrAnnotation)) {
          annotation = optionsOrAnnotation;
        } else {
          options = optionsOrAnnotation;
        }
        let operation;
        let id;
        if (annotation === void 0) {
          operation = CreateFile.create(uri, options);
        } else {
          id = ChangeAnnotationIdentifier.is(annotation) ? annotation : this._changeAnnotations.manage(annotation);
          operation = CreateFile.create(uri, options, id);
        }
        this._workspaceEdit.documentChanges.push(operation);
        if (id !== void 0) {
          return id;
        }
      }
      renameFile(oldUri, newUri, optionsOrAnnotation, options) {
        this.initDocumentChanges();
        if (this._workspaceEdit.documentChanges === void 0) {
          throw new Error("Workspace edit is not configured for document changes.");
        }
        let annotation;
        if (ChangeAnnotation.is(optionsOrAnnotation) || ChangeAnnotationIdentifier.is(optionsOrAnnotation)) {
          annotation = optionsOrAnnotation;
        } else {
          options = optionsOrAnnotation;
        }
        let operation;
        let id;
        if (annotation === void 0) {
          operation = RenameFile.create(oldUri, newUri, options);
        } else {
          id = ChangeAnnotationIdentifier.is(annotation) ? annotation : this._changeAnnotations.manage(annotation);
          operation = RenameFile.create(oldUri, newUri, options, id);
        }
        this._workspaceEdit.documentChanges.push(operation);
        if (id !== void 0) {
          return id;
        }
      }
      deleteFile(uri, optionsOrAnnotation, options) {
        this.initDocumentChanges();
        if (this._workspaceEdit.documentChanges === void 0) {
          throw new Error("Workspace edit is not configured for document changes.");
        }
        let annotation;
        if (ChangeAnnotation.is(optionsOrAnnotation) || ChangeAnnotationIdentifier.is(optionsOrAnnotation)) {
          annotation = optionsOrAnnotation;
        } else {
          options = optionsOrAnnotation;
        }
        let operation;
        let id;
        if (annotation === void 0) {
          operation = DeleteFile.create(uri, options);
        } else {
          id = ChangeAnnotationIdentifier.is(annotation) ? annotation : this._changeAnnotations.manage(annotation);
          operation = DeleteFile.create(uri, options, id);
        }
        this._workspaceEdit.documentChanges.push(operation);
        if (id !== void 0) {
          return id;
        }
      }
    };
    (function(TextDocumentIdentifier2) {
      function create(uri) {
        return { uri };
      }
      TextDocumentIdentifier2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.uri);
      }
      TextDocumentIdentifier2.is = is;
    })(TextDocumentIdentifier || (TextDocumentIdentifier = {}));
    (function(VersionedTextDocumentIdentifier2) {
      function create(uri, version) {
        return { uri, version };
      }
      VersionedTextDocumentIdentifier2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.uri) && Is.integer(candidate.version);
      }
      VersionedTextDocumentIdentifier2.is = is;
    })(VersionedTextDocumentIdentifier || (VersionedTextDocumentIdentifier = {}));
    (function(OptionalVersionedTextDocumentIdentifier2) {
      function create(uri, version) {
        return { uri, version };
      }
      OptionalVersionedTextDocumentIdentifier2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.uri) && (candidate.version === null || Is.integer(candidate.version));
      }
      OptionalVersionedTextDocumentIdentifier2.is = is;
    })(OptionalVersionedTextDocumentIdentifier || (OptionalVersionedTextDocumentIdentifier = {}));
    (function(LanguageKind2) {
      LanguageKind2.ABAP = "abap";
      LanguageKind2.WindowsBat = "bat";
      LanguageKind2.BibTeX = "bibtex";
      LanguageKind2.Clojure = "clojure";
      LanguageKind2.Coffeescript = "coffeescript";
      LanguageKind2.C = "c";
      LanguageKind2.CPP = "cpp";
      LanguageKind2.CSharp = "csharp";
      LanguageKind2.CSS = "css";
      LanguageKind2.D = "d";
      LanguageKind2.Delphi = "pascal";
      LanguageKind2.Diff = "diff";
      LanguageKind2.Dart = "dart";
      LanguageKind2.Dockerfile = "dockerfile";
      LanguageKind2.Elixir = "elixir";
      LanguageKind2.Erlang = "erlang";
      LanguageKind2.FSharp = "fsharp";
      LanguageKind2.GitCommit = "git-commit";
      LanguageKind2.GitRebase = "git-rebase";
      LanguageKind2.Go = "go";
      LanguageKind2.Groovy = "groovy";
      LanguageKind2.Handlebars = "handlebars";
      LanguageKind2.Haskell = "haskell";
      LanguageKind2.HTML = "html";
      LanguageKind2.Ini = "ini";
      LanguageKind2.Java = "java";
      LanguageKind2.JavaScript = "javascript";
      LanguageKind2.JavaScriptReact = "javascriptreact";
      LanguageKind2.JSON = "json";
      LanguageKind2.LaTeX = "latex";
      LanguageKind2.Less = "less";
      LanguageKind2.Lua = "lua";
      LanguageKind2.Makefile = "makefile";
      LanguageKind2.Markdown = "markdown";
      LanguageKind2.ObjectiveC = "objective-c";
      LanguageKind2.ObjectiveCPP = "objective-cpp";
      LanguageKind2.Pascal = "pascal";
      LanguageKind2.Perl = "perl";
      LanguageKind2.Perl6 = "perl6";
      LanguageKind2.PHP = "php";
      LanguageKind2.Plaintext = "plaintext";
      LanguageKind2.Powershell = "powershell";
      LanguageKind2.Pug = "jade";
      LanguageKind2.Python = "python";
      LanguageKind2.R = "r";
      LanguageKind2.Razor = "razor";
      LanguageKind2.Ruby = "ruby";
      LanguageKind2.Rust = "rust";
      LanguageKind2.SCSS = "scss";
      LanguageKind2.SASS = "sass";
      LanguageKind2.Scala = "scala";
      LanguageKind2.ShaderLab = "shaderlab";
      LanguageKind2.ShellScript = "shellscript";
      LanguageKind2.SQL = "sql";
      LanguageKind2.Swift = "swift";
      LanguageKind2.TypeScript = "typescript";
      LanguageKind2.TypeScriptReact = "typescriptreact";
      LanguageKind2.TeX = "tex";
      LanguageKind2.VisualBasic = "vb";
      LanguageKind2.XML = "xml";
      LanguageKind2.XSL = "xsl";
      LanguageKind2.YAML = "yaml";
    })(LanguageKind || (LanguageKind = {}));
    (function(TextDocumentItem2) {
      function create(uri, languageId, version, text) {
        return { uri, languageId, version, text };
      }
      TextDocumentItem2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.uri) && Is.string(candidate.languageId) && Is.integer(candidate.version) && Is.string(candidate.text);
      }
      TextDocumentItem2.is = is;
    })(TextDocumentItem || (TextDocumentItem = {}));
    (function(MarkupKind2) {
      MarkupKind2.PlainText = "plaintext";
      MarkupKind2.Markdown = "markdown";
      function is(value) {
        const candidate = value;
        return candidate === MarkupKind2.PlainText || candidate === MarkupKind2.Markdown;
      }
      MarkupKind2.is = is;
    })(MarkupKind || (MarkupKind = {}));
    (function(MarkupContent2) {
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(value) && MarkupKind.is(candidate.kind) && Is.string(candidate.value);
      }
      MarkupContent2.is = is;
    })(MarkupContent || (MarkupContent = {}));
    (function(CompletionItemKind2) {
      CompletionItemKind2.Text = 1;
      CompletionItemKind2.Method = 2;
      CompletionItemKind2.Function = 3;
      CompletionItemKind2.Constructor = 4;
      CompletionItemKind2.Field = 5;
      CompletionItemKind2.Variable = 6;
      CompletionItemKind2.Class = 7;
      CompletionItemKind2.Interface = 8;
      CompletionItemKind2.Module = 9;
      CompletionItemKind2.Property = 10;
      CompletionItemKind2.Unit = 11;
      CompletionItemKind2.Value = 12;
      CompletionItemKind2.Enum = 13;
      CompletionItemKind2.Keyword = 14;
      CompletionItemKind2.Snippet = 15;
      CompletionItemKind2.Color = 16;
      CompletionItemKind2.File = 17;
      CompletionItemKind2.Reference = 18;
      CompletionItemKind2.Folder = 19;
      CompletionItemKind2.EnumMember = 20;
      CompletionItemKind2.Constant = 21;
      CompletionItemKind2.Struct = 22;
      CompletionItemKind2.Event = 23;
      CompletionItemKind2.Operator = 24;
      CompletionItemKind2.TypeParameter = 25;
    })(CompletionItemKind || (CompletionItemKind = {}));
    (function(InsertTextFormat2) {
      InsertTextFormat2.PlainText = 1;
      InsertTextFormat2.Snippet = 2;
    })(InsertTextFormat || (InsertTextFormat = {}));
    (function(CompletionItemTag2) {
      CompletionItemTag2.Deprecated = 1;
    })(CompletionItemTag || (CompletionItemTag = {}));
    (function(InsertReplaceEdit2) {
      function create(newText, insert, replace) {
        return { newText, insert, replace };
      }
      InsertReplaceEdit2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && Is.string(candidate.newText) && Range.is(candidate.insert) && Range.is(candidate.replace);
      }
      InsertReplaceEdit2.is = is;
    })(InsertReplaceEdit || (InsertReplaceEdit = {}));
    (function(InsertTextMode2) {
      InsertTextMode2.asIs = 1;
      InsertTextMode2.adjustIndentation = 2;
    })(InsertTextMode || (InsertTextMode = {}));
    (function(ApplyKind2) {
      ApplyKind2.Replace = 1;
      ApplyKind2.Merge = 2;
    })(ApplyKind || (ApplyKind = {}));
    (function(CompletionItemLabelDetails2) {
      function is(value) {
        const candidate = value;
        return candidate && (Is.string(candidate.detail) || candidate.detail === void 0) && (Is.string(candidate.description) || candidate.description === void 0);
      }
      CompletionItemLabelDetails2.is = is;
    })(CompletionItemLabelDetails || (CompletionItemLabelDetails = {}));
    (function(CompletionItem2) {
      function create(label) {
        return { label };
      }
      CompletionItem2.create = create;
    })(CompletionItem || (CompletionItem = {}));
    (function(CompletionList2) {
      function create(items, isIncomplete) {
        return { items: items ? items : [], isIncomplete: !!isIncomplete };
      }
      CompletionList2.create = create;
    })(CompletionList || (CompletionList = {}));
    (function(MarkedString2) {
      function fromPlainText(plainText) {
        return plainText.replace(/[\\`*_{}[\]()#+\-.!]/g, "\\$&");
      }
      MarkedString2.fromPlainText = fromPlainText;
      function is(value) {
        const candidate = value;
        return Is.string(candidate) || Is.objectLiteral(candidate) && Is.string(candidate.language) && Is.string(candidate.value);
      }
      MarkedString2.is = is;
    })(MarkedString || (MarkedString = {}));
    (function(Hover2) {
      function is(value) {
        const candidate = value;
        return !!candidate && Is.objectLiteral(candidate) && (MarkupContent.is(candidate.contents) || MarkedString.is(candidate.contents) || Is.typedArray(candidate.contents, MarkedString.is)) && (value.range === void 0 || Range.is(value.range));
      }
      Hover2.is = is;
    })(Hover || (Hover = {}));
    (function(ParameterInformation2) {
      function create(label, documentation) {
        return documentation ? { label, documentation } : { label };
      }
      ParameterInformation2.create = create;
    })(ParameterInformation || (ParameterInformation = {}));
    (function(SignatureInformation2) {
      function create(label, documentation, ...parameters) {
        const result = { label };
        if (Is.defined(documentation)) {
          result.documentation = documentation;
        }
        if (Is.defined(parameters)) {
          result.parameters = parameters;
        } else {
          result.parameters = [];
        }
        return result;
      }
      SignatureInformation2.create = create;
    })(SignatureInformation || (SignatureInformation = {}));
    (function(DocumentHighlightKind2) {
      DocumentHighlightKind2.Text = 1;
      DocumentHighlightKind2.Read = 2;
      DocumentHighlightKind2.Write = 3;
    })(DocumentHighlightKind || (DocumentHighlightKind = {}));
    (function(DocumentHighlight2) {
      function create(range, kind) {
        const result = { range };
        if (Is.number(kind)) {
          result.kind = kind;
        }
        return result;
      }
      DocumentHighlight2.create = create;
    })(DocumentHighlight || (DocumentHighlight = {}));
    (function(SymbolKind2) {
      SymbolKind2.File = 1;
      SymbolKind2.Module = 2;
      SymbolKind2.Namespace = 3;
      SymbolKind2.Package = 4;
      SymbolKind2.Class = 5;
      SymbolKind2.Method = 6;
      SymbolKind2.Property = 7;
      SymbolKind2.Field = 8;
      SymbolKind2.Constructor = 9;
      SymbolKind2.Enum = 10;
      SymbolKind2.Interface = 11;
      SymbolKind2.Function = 12;
      SymbolKind2.Variable = 13;
      SymbolKind2.Constant = 14;
      SymbolKind2.String = 15;
      SymbolKind2.Number = 16;
      SymbolKind2.Boolean = 17;
      SymbolKind2.Array = 18;
      SymbolKind2.Object = 19;
      SymbolKind2.Key = 20;
      SymbolKind2.Null = 21;
      SymbolKind2.EnumMember = 22;
      SymbolKind2.Struct = 23;
      SymbolKind2.Event = 24;
      SymbolKind2.Operator = 25;
      SymbolKind2.TypeParameter = 26;
    })(SymbolKind || (SymbolKind = {}));
    (function(SymbolTag2) {
      SymbolTag2.Deprecated = 1;
    })(SymbolTag || (SymbolTag = {}));
    (function(SymbolInformation2) {
      function create(name, kind, range, uri, containerName) {
        const result = {
          name,
          kind,
          location: { uri, range }
        };
        if (containerName) {
          result.containerName = containerName;
        }
        return result;
      }
      SymbolInformation2.create = create;
    })(SymbolInformation || (SymbolInformation = {}));
    (function(WorkspaceSymbol2) {
      function create(name, kind, uri, range) {
        return range !== void 0 ? { name, kind, location: { uri, range } } : { name, kind, location: { uri } };
      }
      WorkspaceSymbol2.create = create;
    })(WorkspaceSymbol || (WorkspaceSymbol = {}));
    (function(DocumentSymbol2) {
      function create(name, detail, kind, range, selectionRange, children) {
        const result = {
          name,
          detail,
          kind,
          range,
          selectionRange
        };
        if (children !== void 0) {
          result.children = children;
        }
        return result;
      }
      DocumentSymbol2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && Is.string(candidate.name) && Is.number(candidate.kind) && Range.is(candidate.range) && Range.is(candidate.selectionRange) && (candidate.detail === void 0 || Is.string(candidate.detail)) && (candidate.deprecated === void 0 || Is.boolean(candidate.deprecated)) && (candidate.children === void 0 || Array.isArray(candidate.children)) && (candidate.tags === void 0 || Array.isArray(candidate.tags));
      }
      DocumentSymbol2.is = is;
    })(DocumentSymbol || (DocumentSymbol = {}));
    (function(CodeActionKind2) {
      CodeActionKind2.Empty = "";
      CodeActionKind2.QuickFix = "quickfix";
      CodeActionKind2.Refactor = "refactor";
      CodeActionKind2.RefactorExtract = "refactor.extract";
      CodeActionKind2.RefactorInline = "refactor.inline";
      CodeActionKind2.RefactorMove = "refactor.move";
      CodeActionKind2.RefactorRewrite = "refactor.rewrite";
      CodeActionKind2.Source = "source";
      CodeActionKind2.SourceOrganizeImports = "source.organizeImports";
      CodeActionKind2.SourceFixAll = "source.fixAll";
      CodeActionKind2.Notebook = "notebook";
    })(CodeActionKind || (CodeActionKind = {}));
    (function(CodeActionTriggerKind2) {
      CodeActionTriggerKind2.Invoked = 1;
      CodeActionTriggerKind2.Automatic = 2;
    })(CodeActionTriggerKind || (CodeActionTriggerKind = {}));
    (function(CodeActionContext2) {
      function create(diagnostics, only, triggerKind) {
        const result = { diagnostics };
        if (only !== void 0 && only !== null) {
          result.only = only;
        }
        if (triggerKind !== void 0 && triggerKind !== null) {
          result.triggerKind = triggerKind;
        }
        return result;
      }
      CodeActionContext2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.typedArray(candidate.diagnostics, Diagnostic.is) && (candidate.only === void 0 || Is.typedArray(candidate.only, Is.string)) && (candidate.triggerKind === void 0 || candidate.triggerKind === CodeActionTriggerKind.Invoked || candidate.triggerKind === CodeActionTriggerKind.Automatic);
      }
      CodeActionContext2.is = is;
    })(CodeActionContext || (CodeActionContext = {}));
    (function(CodeActionTag2) {
      CodeActionTag2.LLMGenerated = 1;
      function is(value) {
        return Is.defined(value) && value === CodeActionTag2.LLMGenerated;
      }
      CodeActionTag2.is = is;
    })(CodeActionTag || (CodeActionTag = {}));
    (function(CodeAction2) {
      function create(title, kindOrCommandOrEdit, kind) {
        const result = { title };
        let checkKind = true;
        if (typeof kindOrCommandOrEdit === "string") {
          checkKind = false;
          result.kind = kindOrCommandOrEdit;
        } else if (Command.is(kindOrCommandOrEdit)) {
          result.command = kindOrCommandOrEdit;
        } else {
          result.edit = kindOrCommandOrEdit;
        }
        if (checkKind && kind !== void 0) {
          result.kind = kind;
        }
        return result;
      }
      CodeAction2.create = create;
      function is(value) {
        const candidate = value;
        return candidate && Is.string(candidate.title) && (candidate.diagnostics === void 0 || Is.typedArray(candidate.diagnostics, Diagnostic.is)) && (candidate.kind === void 0 || Is.string(candidate.kind)) && (candidate.edit !== void 0 || candidate.command !== void 0) && (candidate.command === void 0 || Command.is(candidate.command)) && (candidate.isPreferred === void 0 || Is.boolean(candidate.isPreferred)) && (candidate.edit === void 0 || WorkspaceEdit.is(candidate.edit)) && (candidate.tags === void 0 || Is.typedArray(candidate.tags, CodeActionTag.is));
      }
      CodeAction2.is = is;
    })(CodeAction || (CodeAction = {}));
    (function(CodeLens2) {
      function create(range, data) {
        const result = { range };
        if (Is.defined(data)) {
          result.data = data;
        }
        return result;
      }
      CodeLens2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Range.is(candidate.range) && (Is.undefined(candidate.command) || Command.is(candidate.command));
      }
      CodeLens2.is = is;
    })(CodeLens || (CodeLens = {}));
    (function(FormattingOptions2) {
      function create(tabSize, insertSpaces) {
        return { tabSize, insertSpaces };
      }
      FormattingOptions2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.uinteger(candidate.tabSize) && Is.boolean(candidate.insertSpaces);
      }
      FormattingOptions2.is = is;
    })(FormattingOptions || (FormattingOptions = {}));
    (function(DocumentLink2) {
      function create(range, target, data) {
        return { range, target, data };
      }
      DocumentLink2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Range.is(candidate.range) && (Is.undefined(candidate.target) || Is.string(candidate.target));
      }
      DocumentLink2.is = is;
    })(DocumentLink || (DocumentLink = {}));
    (function(SelectionRange2) {
      function create(range, parent) {
        return { range, parent };
      }
      SelectionRange2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Range.is(candidate.range) && (candidate.parent === void 0 || SelectionRange2.is(candidate.parent));
      }
      SelectionRange2.is = is;
    })(SelectionRange || (SelectionRange = {}));
    (function(SemanticTokenTypes2) {
      SemanticTokenTypes2["namespace"] = "namespace";
      SemanticTokenTypes2["type"] = "type";
      SemanticTokenTypes2["class"] = "class";
      SemanticTokenTypes2["enum"] = "enum";
      SemanticTokenTypes2["interface"] = "interface";
      SemanticTokenTypes2["struct"] = "struct";
      SemanticTokenTypes2["typeParameter"] = "typeParameter";
      SemanticTokenTypes2["parameter"] = "parameter";
      SemanticTokenTypes2["variable"] = "variable";
      SemanticTokenTypes2["property"] = "property";
      SemanticTokenTypes2["enumMember"] = "enumMember";
      SemanticTokenTypes2["event"] = "event";
      SemanticTokenTypes2["function"] = "function";
      SemanticTokenTypes2["method"] = "method";
      SemanticTokenTypes2["macro"] = "macro";
      SemanticTokenTypes2["keyword"] = "keyword";
      SemanticTokenTypes2["modifier"] = "modifier";
      SemanticTokenTypes2["comment"] = "comment";
      SemanticTokenTypes2["string"] = "string";
      SemanticTokenTypes2["number"] = "number";
      SemanticTokenTypes2["regexp"] = "regexp";
      SemanticTokenTypes2["operator"] = "operator";
      SemanticTokenTypes2["decorator"] = "decorator";
      SemanticTokenTypes2["label"] = "label";
    })(SemanticTokenTypes || (SemanticTokenTypes = {}));
    (function(SemanticTokenModifiers2) {
      SemanticTokenModifiers2["declaration"] = "declaration";
      SemanticTokenModifiers2["definition"] = "definition";
      SemanticTokenModifiers2["readonly"] = "readonly";
      SemanticTokenModifiers2["static"] = "static";
      SemanticTokenModifiers2["deprecated"] = "deprecated";
      SemanticTokenModifiers2["abstract"] = "abstract";
      SemanticTokenModifiers2["async"] = "async";
      SemanticTokenModifiers2["modification"] = "modification";
      SemanticTokenModifiers2["documentation"] = "documentation";
      SemanticTokenModifiers2["defaultLibrary"] = "defaultLibrary";
    })(SemanticTokenModifiers || (SemanticTokenModifiers = {}));
    (function(SemanticTokens2) {
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && (candidate.resultId === void 0 || typeof candidate.resultId === "string") && Array.isArray(candidate.data) && (candidate.data.length === 0 || typeof candidate.data[0] === "number");
      }
      SemanticTokens2.is = is;
    })(SemanticTokens || (SemanticTokens = {}));
    (function(InlineValueText2) {
      function create(range, text) {
        return { range, text };
      }
      InlineValueText2.create = create;
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && Range.is(candidate.range) && Is.string(candidate.text);
      }
      InlineValueText2.is = is;
    })(InlineValueText || (InlineValueText = {}));
    (function(InlineValueVariableLookup2) {
      function create(range, variableName, caseSensitiveLookup) {
        return { range, variableName, caseSensitiveLookup };
      }
      InlineValueVariableLookup2.create = create;
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && Range.is(candidate.range) && Is.boolean(candidate.caseSensitiveLookup) && (Is.string(candidate.variableName) || candidate.variableName === void 0);
      }
      InlineValueVariableLookup2.is = is;
    })(InlineValueVariableLookup || (InlineValueVariableLookup = {}));
    (function(InlineValueEvaluatableExpression2) {
      function create(range, expression) {
        return { range, expression };
      }
      InlineValueEvaluatableExpression2.create = create;
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && Range.is(candidate.range) && (Is.string(candidate.expression) || candidate.expression === void 0);
      }
      InlineValueEvaluatableExpression2.is = is;
    })(InlineValueEvaluatableExpression || (InlineValueEvaluatableExpression = {}));
    (function(InlineValueContext2) {
      function create(frameId, stoppedLocation) {
        return { frameId, stoppedLocation };
      }
      InlineValueContext2.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Range.is(value.stoppedLocation);
      }
      InlineValueContext2.is = is;
    })(InlineValueContext || (InlineValueContext = {}));
    (function(InlayHintKind2) {
      InlayHintKind2.Type = 1;
      InlayHintKind2.Parameter = 2;
      function is(value) {
        return value === 1 || value === 2;
      }
      InlayHintKind2.is = is;
    })(InlayHintKind || (InlayHintKind = {}));
    (function(InlayHintLabelPart2) {
      function create(value) {
        return { value };
      }
      InlayHintLabelPart2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && (candidate.tooltip === void 0 || Is.string(candidate.tooltip) || MarkupContent.is(candidate.tooltip)) && (candidate.location === void 0 || Location.is(candidate.location)) && (candidate.command === void 0 || Command.is(candidate.command));
      }
      InlayHintLabelPart2.is = is;
    })(InlayHintLabelPart || (InlayHintLabelPart = {}));
    (function(InlayHint2) {
      function create(position, label, kind) {
        const result = { position, label };
        if (kind !== void 0) {
          result.kind = kind;
        }
        return result;
      }
      InlayHint2.create = create;
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && Position.is(candidate.position) && (Is.string(candidate.label) || Is.typedArray(candidate.label, InlayHintLabelPart.is)) && (candidate.kind === void 0 || InlayHintKind.is(candidate.kind)) && candidate.textEdits === void 0 || Is.typedArray(candidate.textEdits, TextEdit.is) && (candidate.tooltip === void 0 || Is.string(candidate.tooltip) || MarkupContent.is(candidate.tooltip)) && (candidate.paddingLeft === void 0 || Is.boolean(candidate.paddingLeft)) && (candidate.paddingRight === void 0 || Is.boolean(candidate.paddingRight));
      }
      InlayHint2.is = is;
    })(InlayHint || (InlayHint = {}));
    (function(StringValue2) {
      function createSnippet(value) {
        return { kind: "snippet", value };
      }
      StringValue2.createSnippet = createSnippet;
      function isSnippet(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && candidate.kind === "snippet" && Is.string(candidate.value);
      }
      StringValue2.isSnippet = isSnippet;
    })(StringValue || (StringValue = {}));
    (function(InlineCompletionItem2) {
      function create(insertText, filterText, range, command) {
        return { insertText, filterText, range, command };
      }
      InlineCompletionItem2.create = create;
    })(InlineCompletionItem || (InlineCompletionItem = {}));
    (function(InlineCompletionList2) {
      function create(items) {
        return { items };
      }
      InlineCompletionList2.create = create;
    })(InlineCompletionList || (InlineCompletionList = {}));
    (function(InlineCompletionTriggerKind2) {
      InlineCompletionTriggerKind2.Invoked = 1;
      InlineCompletionTriggerKind2.Automatic = 2;
    })(InlineCompletionTriggerKind || (InlineCompletionTriggerKind = {}));
    (function(SelectedCompletionInfo2) {
      function create(range, text) {
        return { range, text };
      }
      SelectedCompletionInfo2.create = create;
    })(SelectedCompletionInfo || (SelectedCompletionInfo = {}));
    (function(InlineCompletionContext2) {
      function create(triggerKind, selectedCompletionInfo) {
        return { triggerKind, selectedCompletionInfo };
      }
      InlineCompletionContext2.create = create;
    })(InlineCompletionContext || (InlineCompletionContext = {}));
    (function(WorkspaceFolder2) {
      function is(value) {
        const candidate = value;
        return Is.objectLiteral(candidate) && URI.is(candidate.uri) && Is.string(candidate.name);
      }
      WorkspaceFolder2.is = is;
    })(WorkspaceFolder || (WorkspaceFolder = {}));
    EOL = ["\n", "\r\n", "\r"];
    (function(TextDocument3) {
      function create(uri, languageId, version, content) {
        return new FullTextDocument(uri, languageId, version, content);
      }
      TextDocument3.create = create;
      function is(value) {
        const candidate = value;
        return Is.defined(candidate) && Is.string(candidate.uri) && (Is.undefined(candidate.languageId) || Is.string(candidate.languageId)) && Is.uinteger(candidate.lineCount) && Is.func(candidate.getText) && Is.func(candidate.positionAt) && Is.func(candidate.offsetAt) ? true : false;
      }
      TextDocument3.is = is;
      function applyEdits(document, edits) {
        let text = document.getText();
        const sortedEdits = mergeSort2(edits, (a, b) => {
          const diff = a.range.start.line - b.range.start.line;
          if (diff === 0) {
            return a.range.start.character - b.range.start.character;
          }
          return diff;
        });
        let lastModifiedOffset = text.length;
        for (let i = sortedEdits.length - 1; i >= 0; i--) {
          const e = sortedEdits[i];
          const startOffset = document.offsetAt(e.range.start);
          const endOffset = document.offsetAt(e.range.end);
          if (endOffset <= lastModifiedOffset) {
            text = text.substring(0, startOffset) + e.newText + text.substring(endOffset, text.length);
          } else {
            throw new Error("Overlapping edit");
          }
          lastModifiedOffset = startOffset;
        }
        return text;
      }
      TextDocument3.applyEdits = applyEdits;
      function mergeSort2(data, compare) {
        if (data.length <= 1) {
          return data;
        }
        const p = data.length / 2 | 0;
        const left = data.slice(0, p);
        const right = data.slice(p);
        mergeSort2(left, compare);
        mergeSort2(right, compare);
        let leftIdx = 0;
        let rightIdx = 0;
        let i = 0;
        while (leftIdx < left.length && rightIdx < right.length) {
          const ret = compare(left[leftIdx], right[rightIdx]);
          if (ret <= 0) {
            data[i++] = left[leftIdx++];
          } else {
            data[i++] = right[rightIdx++];
          }
        }
        while (leftIdx < left.length) {
          data[i++] = left[leftIdx++];
        }
        while (rightIdx < right.length) {
          data[i++] = right[rightIdx++];
        }
        return data;
      }
    })(TextDocument || (TextDocument = {}));
    FullTextDocument = class {
      constructor(uri, languageId, version, content) {
        this._uri = uri;
        this._languageId = languageId;
        this._version = version;
        this._content = content;
        this._lineOffsets = void 0;
      }
      get uri() {
        return this._uri;
      }
      get languageId() {
        return this._languageId;
      }
      get version() {
        return this._version;
      }
      getText(range) {
        if (range) {
          const start = this.offsetAt(range.start);
          const end = this.offsetAt(range.end);
          return this._content.substring(start, end);
        }
        return this._content;
      }
      update(event, version) {
        this._content = event.text;
        this._version = version;
        this._lineOffsets = void 0;
      }
      getLineOffsets() {
        if (this._lineOffsets === void 0) {
          const lineOffsets = [];
          const text = this._content;
          let isLineStart = true;
          for (let i = 0; i < text.length; i++) {
            if (isLineStart) {
              lineOffsets.push(i);
              isLineStart = false;
            }
            const ch = text.charAt(i);
            isLineStart = ch === "\r" || ch === "\n";
            if (ch === "\r" && i + 1 < text.length && text.charAt(i + 1) === "\n") {
              i++;
            }
          }
          if (isLineStart && text.length > 0) {
            lineOffsets.push(text.length);
          }
          this._lineOffsets = lineOffsets;
        }
        return this._lineOffsets;
      }
      positionAt(offset) {
        offset = Math.max(Math.min(offset, this._content.length), 0);
        const lineOffsets = this.getLineOffsets();
        let low = 0, high = lineOffsets.length;
        if (high === 0) {
          return Position.create(0, offset);
        }
        while (low < high) {
          const mid = Math.floor((low + high) / 2);
          if (lineOffsets[mid] > offset) {
            high = mid;
          } else {
            low = mid + 1;
          }
        }
        const line = low - 1;
        return Position.create(line, offset - lineOffsets[line]);
      }
      offsetAt(position) {
        const lineOffsets = this.getLineOffsets();
        if (position.line >= lineOffsets.length) {
          return this._content.length;
        } else if (position.line < 0) {
          return 0;
        }
        const lineOffset = lineOffsets[position.line];
        const nextLineOffset = position.line + 1 < lineOffsets.length ? lineOffsets[position.line + 1] : this._content.length;
        return Math.max(Math.min(lineOffset + position.character, nextLineOffset), lineOffset);
      }
      get lineCount() {
        return this.getLineOffsets().length;
      }
    };
    (function(Is2) {
      const toString = Object.prototype.toString;
      function defined(value) {
        return typeof value !== "undefined";
      }
      Is2.defined = defined;
      function undefined2(value) {
        return typeof value === "undefined";
      }
      Is2.undefined = undefined2;
      function boolean(value) {
        return value === true || value === false;
      }
      Is2.boolean = boolean;
      function string(value) {
        return toString.call(value) === "[object String]";
      }
      Is2.string = string;
      function number(value) {
        return toString.call(value) === "[object Number]";
      }
      Is2.number = number;
      function numberRange(value, min, max) {
        return toString.call(value) === "[object Number]" && min <= value && value <= max;
      }
      Is2.numberRange = numberRange;
      function integer2(value) {
        return toString.call(value) === "[object Number]" && -2147483648 <= value && value <= 2147483647;
      }
      Is2.integer = integer2;
      function uinteger2(value) {
        return toString.call(value) === "[object Number]" && 0 <= value && value <= 2147483647;
      }
      Is2.uinteger = uinteger2;
      function func(value) {
        return toString.call(value) === "[object Function]";
      }
      Is2.func = func;
      function objectLiteral(value) {
        return value !== null && typeof value === "object";
      }
      Is2.objectLiteral = objectLiteral;
      function typedArray(value, check) {
        return Array.isArray(value) && value.every(check);
      }
      Is2.typedArray = typedArray;
    })(Is || (Is = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/messages.js
var require_messages2 = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/messages.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CM = exports2.ProtocolNotificationType = exports2.ProtocolNotificationType0 = exports2.ProtocolRequestType = exports2.ProtocolRequestType0 = exports2.RegistrationType = exports2.MessageDirection = void 0;
    var vscode_jsonrpc_1 = require_api();
    var MessageDirection;
    (function(MessageDirection2) {
      MessageDirection2["clientToServer"] = "clientToServer";
      MessageDirection2["serverToClient"] = "serverToClient";
      MessageDirection2["both"] = "both";
    })(MessageDirection || (exports2.MessageDirection = MessageDirection = {}));
    var RegistrationType = class {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      ____;
      method;
      constructor(method) {
        this.method = method;
      }
    };
    exports2.RegistrationType = RegistrationType;
    var ProtocolRequestType0 = class extends vscode_jsonrpc_1.RequestType0 {
      /**
       * Clients must not use these properties. They are here to ensure correct typing.
       * in TypeScript
       */
      __;
      ___;
      ____;
      _pr;
      constructor(method) {
        super(method);
      }
    };
    exports2.ProtocolRequestType0 = ProtocolRequestType0;
    var ProtocolRequestType = class extends vscode_jsonrpc_1.RequestType {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      __;
      ___;
      ____;
      _pr;
      constructor(method) {
        super(method, vscode_jsonrpc_1.ParameterStructures.byName);
      }
    };
    exports2.ProtocolRequestType = ProtocolRequestType;
    var ProtocolNotificationType0 = class extends vscode_jsonrpc_1.NotificationType0 {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      ___;
      ____;
      constructor(method) {
        super(method);
      }
    };
    exports2.ProtocolNotificationType0 = ProtocolNotificationType0;
    var ProtocolNotificationType = class extends vscode_jsonrpc_1.NotificationType {
      /**
       * Clients must not use this property. It is here to ensure correct typing.
       */
      ___;
      ____;
      constructor(method) {
        super(method, vscode_jsonrpc_1.ParameterStructures.byName);
      }
    };
    exports2.ProtocolNotificationType = ProtocolNotificationType;
    var CM;
    (function(CM2) {
      function create(client, server) {
        return { client, server };
      }
      CM2.create = create;
    })(CM || (exports2.CM = CM = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/utils/is.js
var require_is3 = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/utils/is.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.boolean = boolean;
    exports2.string = string;
    exports2.number = number;
    exports2.error = error;
    exports2.func = func;
    exports2.array = array;
    exports2.stringArray = stringArray;
    exports2.typedArray = typedArray;
    exports2.objectLiteral = objectLiteral;
    function boolean(value) {
      return value === true || value === false;
    }
    function string(value) {
      return typeof value === "string" || value instanceof String;
    }
    function number(value) {
      return typeof value === "number" || value instanceof Number;
    }
    function error(value) {
      return value instanceof Error;
    }
    function func(value) {
      return typeof value === "function";
    }
    function array(value) {
      return Array.isArray(value);
    }
    function stringArray(value) {
      return array(value) && value.every((elem) => string(elem));
    }
    function typedArray(value, check) {
      return Array.isArray(value) && value.every(check);
    }
    function objectLiteral(value) {
      return value !== null && typeof value === "object";
    }
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.implementation.js
var require_protocol_implementation = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.implementation.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ImplementationRequest = void 0;
    var messages_1 = require_messages2();
    var ImplementationRequest;
    (function(ImplementationRequest2) {
      ImplementationRequest2.method = "textDocument/implementation";
      ImplementationRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      ImplementationRequest2.type = new messages_1.ProtocolRequestType(ImplementationRequest2.method);
      ImplementationRequest2.capabilities = messages_1.CM.create("textDocument.implementation", "implementationProvider");
    })(ImplementationRequest || (exports2.ImplementationRequest = ImplementationRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.typeDefinition.js
var require_protocol_typeDefinition = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.typeDefinition.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TypeDefinitionRequest = void 0;
    var messages_1 = require_messages2();
    var TypeDefinitionRequest;
    (function(TypeDefinitionRequest2) {
      TypeDefinitionRequest2.method = "textDocument/typeDefinition";
      TypeDefinitionRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      TypeDefinitionRequest2.type = new messages_1.ProtocolRequestType(TypeDefinitionRequest2.method);
      TypeDefinitionRequest2.capabilities = messages_1.CM.create("textDocument.typeDefinition", "typeDefinitionProvider");
    })(TypeDefinitionRequest || (exports2.TypeDefinitionRequest = TypeDefinitionRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.workspaceFolder.js
var require_protocol_workspaceFolder = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.workspaceFolder.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DidChangeWorkspaceFoldersNotification = exports2.WorkspaceFoldersRequest = void 0;
    var messages_1 = require_messages2();
    var WorkspaceFoldersRequest;
    (function(WorkspaceFoldersRequest2) {
      WorkspaceFoldersRequest2.method = "workspace/workspaceFolders";
      WorkspaceFoldersRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      WorkspaceFoldersRequest2.type = new messages_1.ProtocolRequestType0(WorkspaceFoldersRequest2.method);
      WorkspaceFoldersRequest2.capabilities = messages_1.CM.create("workspace.workspaceFolders", "workspace.workspaceFolders");
    })(WorkspaceFoldersRequest || (exports2.WorkspaceFoldersRequest = WorkspaceFoldersRequest = {}));
    var DidChangeWorkspaceFoldersNotification;
    (function(DidChangeWorkspaceFoldersNotification2) {
      DidChangeWorkspaceFoldersNotification2.method = "workspace/didChangeWorkspaceFolders";
      DidChangeWorkspaceFoldersNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidChangeWorkspaceFoldersNotification2.type = new messages_1.ProtocolNotificationType(DidChangeWorkspaceFoldersNotification2.method);
      DidChangeWorkspaceFoldersNotification2.capabilities = messages_1.CM.create(void 0, "workspace.workspaceFolders.changeNotifications");
    })(DidChangeWorkspaceFoldersNotification || (exports2.DidChangeWorkspaceFoldersNotification = DidChangeWorkspaceFoldersNotification = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.configuration.js
var require_protocol_configuration = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.configuration.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ConfigurationRequest = void 0;
    var messages_1 = require_messages2();
    var ConfigurationRequest;
    (function(ConfigurationRequest2) {
      ConfigurationRequest2.method = "workspace/configuration";
      ConfigurationRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      ConfigurationRequest2.type = new messages_1.ProtocolRequestType(ConfigurationRequest2.method);
      ConfigurationRequest2.capabilities = messages_1.CM.create("workspace.configuration", void 0);
    })(ConfigurationRequest || (exports2.ConfigurationRequest = ConfigurationRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.colorProvider.js
var require_protocol_colorProvider = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.colorProvider.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ColorPresentationRequest = exports2.DocumentColorRequest = void 0;
    var messages_1 = require_messages2();
    var DocumentColorRequest;
    (function(DocumentColorRequest2) {
      DocumentColorRequest2.method = "textDocument/documentColor";
      DocumentColorRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentColorRequest2.type = new messages_1.ProtocolRequestType(DocumentColorRequest2.method);
      DocumentColorRequest2.capabilities = messages_1.CM.create("textDocument.colorProvider", "colorProvider");
    })(DocumentColorRequest || (exports2.DocumentColorRequest = DocumentColorRequest = {}));
    var ColorPresentationRequest;
    (function(ColorPresentationRequest2) {
      ColorPresentationRequest2.method = "textDocument/colorPresentation";
      ColorPresentationRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      ColorPresentationRequest2.type = new messages_1.ProtocolRequestType(ColorPresentationRequest2.method);
      ColorPresentationRequest2.capabilities = messages_1.CM.create("textDocument.colorProvider", "colorProvider");
    })(ColorPresentationRequest || (exports2.ColorPresentationRequest = ColorPresentationRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.foldingRange.js
var require_protocol_foldingRange = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.foldingRange.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.FoldingRangeRefreshRequest = exports2.FoldingRangeRequest = void 0;
    var messages_1 = require_messages2();
    var FoldingRangeRequest;
    (function(FoldingRangeRequest2) {
      FoldingRangeRequest2.method = "textDocument/foldingRange";
      FoldingRangeRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      FoldingRangeRequest2.type = new messages_1.ProtocolRequestType(FoldingRangeRequest2.method);
      FoldingRangeRequest2.capabilities = messages_1.CM.create("textDocument.foldingRange", "foldingRangeProvider");
    })(FoldingRangeRequest || (exports2.FoldingRangeRequest = FoldingRangeRequest = {}));
    var FoldingRangeRefreshRequest;
    (function(FoldingRangeRefreshRequest2) {
      FoldingRangeRefreshRequest2.method = `workspace/foldingRange/refresh`;
      FoldingRangeRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      FoldingRangeRefreshRequest2.type = new messages_1.ProtocolRequestType0(FoldingRangeRefreshRequest2.method);
      FoldingRangeRefreshRequest2.capabilities = messages_1.CM.create("workspace.foldingRange.refreshSupport", void 0);
    })(FoldingRangeRefreshRequest || (exports2.FoldingRangeRefreshRequest = FoldingRangeRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.declaration.js
var require_protocol_declaration = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.declaration.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DeclarationRequest = void 0;
    var messages_1 = require_messages2();
    var DeclarationRequest;
    (function(DeclarationRequest2) {
      DeclarationRequest2.method = "textDocument/declaration";
      DeclarationRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DeclarationRequest2.type = new messages_1.ProtocolRequestType(DeclarationRequest2.method);
      DeclarationRequest2.capabilities = messages_1.CM.create("textDocument.declaration", "declarationProvider");
    })(DeclarationRequest || (exports2.DeclarationRequest = DeclarationRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.selectionRange.js
var require_protocol_selectionRange = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.selectionRange.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SelectionRangeRequest = void 0;
    var messages_1 = require_messages2();
    var SelectionRangeRequest;
    (function(SelectionRangeRequest2) {
      SelectionRangeRequest2.method = "textDocument/selectionRange";
      SelectionRangeRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      SelectionRangeRequest2.type = new messages_1.ProtocolRequestType(SelectionRangeRequest2.method);
      SelectionRangeRequest2.capabilities = messages_1.CM.create("textDocument.selectionRange", "selectionRangeProvider");
    })(SelectionRangeRequest || (exports2.SelectionRangeRequest = SelectionRangeRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.progress.js
var require_protocol_progress = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.progress.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WorkDoneProgressCancelNotification = exports2.WorkDoneProgressCreateRequest = exports2.WorkDoneProgress = void 0;
    var vscode_jsonrpc_1 = require_api();
    var messages_1 = require_messages2();
    var WorkDoneProgress;
    (function(WorkDoneProgress2) {
      WorkDoneProgress2.type = new vscode_jsonrpc_1.ProgressType();
      function is(value) {
        return value === WorkDoneProgress2.type;
      }
      WorkDoneProgress2.is = is;
    })(WorkDoneProgress || (exports2.WorkDoneProgress = WorkDoneProgress = {}));
    var WorkDoneProgressCreateRequest;
    (function(WorkDoneProgressCreateRequest2) {
      WorkDoneProgressCreateRequest2.method = "window/workDoneProgress/create";
      WorkDoneProgressCreateRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      WorkDoneProgressCreateRequest2.type = new messages_1.ProtocolRequestType(WorkDoneProgressCreateRequest2.method);
      WorkDoneProgressCreateRequest2.capabilities = messages_1.CM.create("window.workDoneProgress", void 0);
    })(WorkDoneProgressCreateRequest || (exports2.WorkDoneProgressCreateRequest = WorkDoneProgressCreateRequest = {}));
    var WorkDoneProgressCancelNotification;
    (function(WorkDoneProgressCancelNotification2) {
      WorkDoneProgressCancelNotification2.method = "window/workDoneProgress/cancel";
      WorkDoneProgressCancelNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      WorkDoneProgressCancelNotification2.type = new messages_1.ProtocolNotificationType(WorkDoneProgressCancelNotification2.method);
    })(WorkDoneProgressCancelNotification || (exports2.WorkDoneProgressCancelNotification = WorkDoneProgressCancelNotification = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.callHierarchy.js
var require_protocol_callHierarchy = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.callHierarchy.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CallHierarchyOutgoingCallsRequest = exports2.CallHierarchyIncomingCallsRequest = exports2.CallHierarchyPrepareRequest = void 0;
    var messages_1 = require_messages2();
    var CallHierarchyPrepareRequest;
    (function(CallHierarchyPrepareRequest2) {
      CallHierarchyPrepareRequest2.method = "textDocument/prepareCallHierarchy";
      CallHierarchyPrepareRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CallHierarchyPrepareRequest2.type = new messages_1.ProtocolRequestType(CallHierarchyPrepareRequest2.method);
      CallHierarchyPrepareRequest2.capabilities = messages_1.CM.create("textDocument.callHierarchy", "callHierarchyProvider");
    })(CallHierarchyPrepareRequest || (exports2.CallHierarchyPrepareRequest = CallHierarchyPrepareRequest = {}));
    var CallHierarchyIncomingCallsRequest;
    (function(CallHierarchyIncomingCallsRequest2) {
      CallHierarchyIncomingCallsRequest2.method = "callHierarchy/incomingCalls";
      CallHierarchyIncomingCallsRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CallHierarchyIncomingCallsRequest2.type = new messages_1.ProtocolRequestType(CallHierarchyIncomingCallsRequest2.method);
      CallHierarchyIncomingCallsRequest2.capabilities = messages_1.CM.create("textDocument.callHierarchy", "callHierarchyProvider");
    })(CallHierarchyIncomingCallsRequest || (exports2.CallHierarchyIncomingCallsRequest = CallHierarchyIncomingCallsRequest = {}));
    var CallHierarchyOutgoingCallsRequest;
    (function(CallHierarchyOutgoingCallsRequest2) {
      CallHierarchyOutgoingCallsRequest2.method = "callHierarchy/outgoingCalls";
      CallHierarchyOutgoingCallsRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CallHierarchyOutgoingCallsRequest2.type = new messages_1.ProtocolRequestType(CallHierarchyOutgoingCallsRequest2.method);
      CallHierarchyOutgoingCallsRequest2.capabilities = messages_1.CM.create("textDocument.callHierarchy", "callHierarchyProvider");
    })(CallHierarchyOutgoingCallsRequest || (exports2.CallHierarchyOutgoingCallsRequest = CallHierarchyOutgoingCallsRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.semanticTokens.js
var require_protocol_semanticTokens = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.semanticTokens.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SemanticTokensRefreshRequest = exports2.SemanticTokensRangeRequest = exports2.SemanticTokensDeltaRequest = exports2.SemanticTokensRequest = exports2.SemanticTokensRegistrationType = exports2.TokenFormat = void 0;
    var messages_1 = require_messages2();
    var TokenFormat;
    (function(TokenFormat2) {
      TokenFormat2.Relative = "relative";
    })(TokenFormat || (exports2.TokenFormat = TokenFormat = {}));
    var SemanticTokensRegistrationType;
    (function(SemanticTokensRegistrationType2) {
      SemanticTokensRegistrationType2.method = "textDocument/semanticTokens";
      SemanticTokensRegistrationType2.type = new messages_1.RegistrationType(SemanticTokensRegistrationType2.method);
    })(SemanticTokensRegistrationType || (exports2.SemanticTokensRegistrationType = SemanticTokensRegistrationType = {}));
    var SemanticTokensRequest;
    (function(SemanticTokensRequest2) {
      SemanticTokensRequest2.method = "textDocument/semanticTokens/full";
      SemanticTokensRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      SemanticTokensRequest2.type = new messages_1.ProtocolRequestType(SemanticTokensRequest2.method);
      SemanticTokensRequest2.registrationMethod = SemanticTokensRegistrationType.method;
      SemanticTokensRequest2.capabilities = messages_1.CM.create("textDocument.semanticTokens", "semanticTokensProvider");
    })(SemanticTokensRequest || (exports2.SemanticTokensRequest = SemanticTokensRequest = {}));
    var SemanticTokensDeltaRequest;
    (function(SemanticTokensDeltaRequest2) {
      SemanticTokensDeltaRequest2.method = "textDocument/semanticTokens/full/delta";
      SemanticTokensDeltaRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      SemanticTokensDeltaRequest2.type = new messages_1.ProtocolRequestType(SemanticTokensDeltaRequest2.method);
      SemanticTokensDeltaRequest2.registrationMethod = SemanticTokensRegistrationType.method;
      SemanticTokensDeltaRequest2.capabilities = messages_1.CM.create("textDocument.semanticTokens.requests.full.delta", "semanticTokensProvider.full.delta");
    })(SemanticTokensDeltaRequest || (exports2.SemanticTokensDeltaRequest = SemanticTokensDeltaRequest = {}));
    var SemanticTokensRangeRequest;
    (function(SemanticTokensRangeRequest2) {
      SemanticTokensRangeRequest2.method = "textDocument/semanticTokens/range";
      SemanticTokensRangeRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      SemanticTokensRangeRequest2.type = new messages_1.ProtocolRequestType(SemanticTokensRangeRequest2.method);
      SemanticTokensRangeRequest2.registrationMethod = SemanticTokensRegistrationType.method;
      SemanticTokensRangeRequest2.capabilities = messages_1.CM.create("textDocument.semanticTokens.requests.range", "semanticTokensProvider.range");
    })(SemanticTokensRangeRequest || (exports2.SemanticTokensRangeRequest = SemanticTokensRangeRequest = {}));
    var SemanticTokensRefreshRequest;
    (function(SemanticTokensRefreshRequest2) {
      SemanticTokensRefreshRequest2.method = `workspace/semanticTokens/refresh`;
      SemanticTokensRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      SemanticTokensRefreshRequest2.type = new messages_1.ProtocolRequestType0(SemanticTokensRefreshRequest2.method);
      SemanticTokensRefreshRequest2.capabilities = messages_1.CM.create("workspace.semanticTokens.refreshSupport", void 0);
    })(SemanticTokensRefreshRequest || (exports2.SemanticTokensRefreshRequest = SemanticTokensRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.showDocument.js
var require_protocol_showDocument = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.showDocument.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ShowDocumentRequest = void 0;
    var messages_1 = require_messages2();
    var ShowDocumentRequest;
    (function(ShowDocumentRequest2) {
      ShowDocumentRequest2.method = "window/showDocument";
      ShowDocumentRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      ShowDocumentRequest2.type = new messages_1.ProtocolRequestType(ShowDocumentRequest2.method);
      ShowDocumentRequest2.capabilities = messages_1.CM.create("window.showDocument.support", void 0);
    })(ShowDocumentRequest || (exports2.ShowDocumentRequest = ShowDocumentRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.linkedEditingRange.js
var require_protocol_linkedEditingRange = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.linkedEditingRange.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.LinkedEditingRangeRequest = void 0;
    var messages_1 = require_messages2();
    var LinkedEditingRangeRequest;
    (function(LinkedEditingRangeRequest2) {
      LinkedEditingRangeRequest2.method = "textDocument/linkedEditingRange";
      LinkedEditingRangeRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      LinkedEditingRangeRequest2.type = new messages_1.ProtocolRequestType(LinkedEditingRangeRequest2.method);
      LinkedEditingRangeRequest2.capabilities = messages_1.CM.create("textDocument.linkedEditingRange", "linkedEditingRangeProvider");
    })(LinkedEditingRangeRequest || (exports2.LinkedEditingRangeRequest = LinkedEditingRangeRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.fileOperations.js
var require_protocol_fileOperations = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.fileOperations.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WillDeleteFilesRequest = exports2.DidDeleteFilesNotification = exports2.DidRenameFilesNotification = exports2.WillRenameFilesRequest = exports2.DidCreateFilesNotification = exports2.WillCreateFilesRequest = exports2.FileOperationPatternKind = void 0;
    var messages_1 = require_messages2();
    var FileOperationPatternKind;
    (function(FileOperationPatternKind2) {
      FileOperationPatternKind2.file = "file";
      FileOperationPatternKind2.folder = "folder";
    })(FileOperationPatternKind || (exports2.FileOperationPatternKind = FileOperationPatternKind = {}));
    var WillCreateFilesRequest;
    (function(WillCreateFilesRequest2) {
      WillCreateFilesRequest2.method = "workspace/willCreateFiles";
      WillCreateFilesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WillCreateFilesRequest2.type = new messages_1.ProtocolRequestType(WillCreateFilesRequest2.method);
      WillCreateFilesRequest2.capabilities = messages_1.CM.create("workspace.fileOperations.willCreate", "workspace.fileOperations.willCreate");
    })(WillCreateFilesRequest || (exports2.WillCreateFilesRequest = WillCreateFilesRequest = {}));
    var DidCreateFilesNotification;
    (function(DidCreateFilesNotification2) {
      DidCreateFilesNotification2.method = "workspace/didCreateFiles";
      DidCreateFilesNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidCreateFilesNotification2.type = new messages_1.ProtocolNotificationType(DidCreateFilesNotification2.method);
      DidCreateFilesNotification2.capabilities = messages_1.CM.create("workspace.fileOperations.didCreate", "workspace.fileOperations.didCreate");
    })(DidCreateFilesNotification || (exports2.DidCreateFilesNotification = DidCreateFilesNotification = {}));
    var WillRenameFilesRequest;
    (function(WillRenameFilesRequest2) {
      WillRenameFilesRequest2.method = "workspace/willRenameFiles";
      WillRenameFilesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WillRenameFilesRequest2.type = new messages_1.ProtocolRequestType(WillRenameFilesRequest2.method);
      WillRenameFilesRequest2.capabilities = messages_1.CM.create("workspace.fileOperations.willRename", "workspace.fileOperations.willRename");
    })(WillRenameFilesRequest || (exports2.WillRenameFilesRequest = WillRenameFilesRequest = {}));
    var DidRenameFilesNotification;
    (function(DidRenameFilesNotification2) {
      DidRenameFilesNotification2.method = "workspace/didRenameFiles";
      DidRenameFilesNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidRenameFilesNotification2.type = new messages_1.ProtocolNotificationType(DidRenameFilesNotification2.method);
      DidRenameFilesNotification2.capabilities = messages_1.CM.create("workspace.fileOperations.didRename", "workspace.fileOperations.didRename");
    })(DidRenameFilesNotification || (exports2.DidRenameFilesNotification = DidRenameFilesNotification = {}));
    var DidDeleteFilesNotification;
    (function(DidDeleteFilesNotification2) {
      DidDeleteFilesNotification2.method = "workspace/didDeleteFiles";
      DidDeleteFilesNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidDeleteFilesNotification2.type = new messages_1.ProtocolNotificationType(DidDeleteFilesNotification2.method);
      DidDeleteFilesNotification2.capabilities = messages_1.CM.create("workspace.fileOperations.didDelete", "workspace.fileOperations.didDelete");
    })(DidDeleteFilesNotification || (exports2.DidDeleteFilesNotification = DidDeleteFilesNotification = {}));
    var WillDeleteFilesRequest;
    (function(WillDeleteFilesRequest2) {
      WillDeleteFilesRequest2.method = "workspace/willDeleteFiles";
      WillDeleteFilesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WillDeleteFilesRequest2.type = new messages_1.ProtocolRequestType(WillDeleteFilesRequest2.method);
      WillDeleteFilesRequest2.capabilities = messages_1.CM.create("workspace.fileOperations.willDelete", "workspace.fileOperations.willDelete");
    })(WillDeleteFilesRequest || (exports2.WillDeleteFilesRequest = WillDeleteFilesRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.moniker.js
var require_protocol_moniker = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.moniker.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.MonikerRequest = exports2.MonikerKind = exports2.UniquenessLevel = void 0;
    var messages_1 = require_messages2();
    var UniquenessLevel;
    (function(UniquenessLevel2) {
      UniquenessLevel2.document = "document";
      UniquenessLevel2.project = "project";
      UniquenessLevel2.group = "group";
      UniquenessLevel2.scheme = "scheme";
      UniquenessLevel2.global = "global";
    })(UniquenessLevel || (exports2.UniquenessLevel = UniquenessLevel = {}));
    var MonikerKind;
    (function(MonikerKind2) {
      MonikerKind2.$import = "import";
      MonikerKind2.$export = "export";
      MonikerKind2.local = "local";
    })(MonikerKind || (exports2.MonikerKind = MonikerKind = {}));
    var MonikerRequest;
    (function(MonikerRequest2) {
      MonikerRequest2.method = "textDocument/moniker";
      MonikerRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      MonikerRequest2.type = new messages_1.ProtocolRequestType(MonikerRequest2.method);
      MonikerRequest2.capabilities = messages_1.CM.create("textDocument.moniker", "monikerProvider");
    })(MonikerRequest || (exports2.MonikerRequest = MonikerRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.typeHierarchy.js
var require_protocol_typeHierarchy = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.typeHierarchy.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TypeHierarchySubtypesRequest = exports2.TypeHierarchySupertypesRequest = exports2.TypeHierarchyPrepareRequest = void 0;
    var messages_1 = require_messages2();
    var TypeHierarchyPrepareRequest;
    (function(TypeHierarchyPrepareRequest2) {
      TypeHierarchyPrepareRequest2.method = "textDocument/prepareTypeHierarchy";
      TypeHierarchyPrepareRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      TypeHierarchyPrepareRequest2.type = new messages_1.ProtocolRequestType(TypeHierarchyPrepareRequest2.method);
      TypeHierarchyPrepareRequest2.capabilities = messages_1.CM.create("textDocument.typeHierarchy", "typeHierarchyProvider");
    })(TypeHierarchyPrepareRequest || (exports2.TypeHierarchyPrepareRequest = TypeHierarchyPrepareRequest = {}));
    var TypeHierarchySupertypesRequest;
    (function(TypeHierarchySupertypesRequest2) {
      TypeHierarchySupertypesRequest2.method = "typeHierarchy/supertypes";
      TypeHierarchySupertypesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      TypeHierarchySupertypesRequest2.type = new messages_1.ProtocolRequestType(TypeHierarchySupertypesRequest2.method);
    })(TypeHierarchySupertypesRequest || (exports2.TypeHierarchySupertypesRequest = TypeHierarchySupertypesRequest = {}));
    var TypeHierarchySubtypesRequest;
    (function(TypeHierarchySubtypesRequest2) {
      TypeHierarchySubtypesRequest2.method = "typeHierarchy/subtypes";
      TypeHierarchySubtypesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      TypeHierarchySubtypesRequest2.type = new messages_1.ProtocolRequestType(TypeHierarchySubtypesRequest2.method);
    })(TypeHierarchySubtypesRequest || (exports2.TypeHierarchySubtypesRequest = TypeHierarchySubtypesRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.inlineValue.js
var require_protocol_inlineValue = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.inlineValue.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlineValueRefreshRequest = exports2.InlineValueRequest = void 0;
    var messages_1 = require_messages2();
    var InlineValueRequest;
    (function(InlineValueRequest2) {
      InlineValueRequest2.method = "textDocument/inlineValue";
      InlineValueRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      InlineValueRequest2.type = new messages_1.ProtocolRequestType(InlineValueRequest2.method);
      InlineValueRequest2.capabilities = messages_1.CM.create("textDocument.inlineValue", "inlineValueProvider");
    })(InlineValueRequest || (exports2.InlineValueRequest = InlineValueRequest = {}));
    var InlineValueRefreshRequest;
    (function(InlineValueRefreshRequest2) {
      InlineValueRefreshRequest2.method = `workspace/inlineValue/refresh`;
      InlineValueRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      InlineValueRefreshRequest2.type = new messages_1.ProtocolRequestType0(InlineValueRefreshRequest2.method);
      InlineValueRefreshRequest2.capabilities = messages_1.CM.create("workspace.inlineValue.refreshSupport", void 0);
    })(InlineValueRefreshRequest || (exports2.InlineValueRefreshRequest = InlineValueRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.inlayHint.js
var require_protocol_inlayHint = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.inlayHint.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlayHintRefreshRequest = exports2.InlayHintResolveRequest = exports2.InlayHintRequest = void 0;
    var messages_1 = require_messages2();
    var InlayHintRequest;
    (function(InlayHintRequest2) {
      InlayHintRequest2.method = "textDocument/inlayHint";
      InlayHintRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      InlayHintRequest2.type = new messages_1.ProtocolRequestType(InlayHintRequest2.method);
      InlayHintRequest2.capabilities = messages_1.CM.create("textDocument.inlayHint", "inlayHintProvider");
    })(InlayHintRequest || (exports2.InlayHintRequest = InlayHintRequest = {}));
    var InlayHintResolveRequest;
    (function(InlayHintResolveRequest2) {
      InlayHintResolveRequest2.method = "inlayHint/resolve";
      InlayHintResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      InlayHintResolveRequest2.type = new messages_1.ProtocolRequestType(InlayHintResolveRequest2.method);
      InlayHintResolveRequest2.capabilities = messages_1.CM.create("textDocument.inlayHint.resolveSupport", "inlayHintProvider.resolveProvider");
    })(InlayHintResolveRequest || (exports2.InlayHintResolveRequest = InlayHintResolveRequest = {}));
    var InlayHintRefreshRequest;
    (function(InlayHintRefreshRequest2) {
      InlayHintRefreshRequest2.method = `workspace/inlayHint/refresh`;
      InlayHintRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      InlayHintRefreshRequest2.type = new messages_1.ProtocolRequestType0(InlayHintRefreshRequest2.method);
      InlayHintRefreshRequest2.capabilities = messages_1.CM.create("workspace.inlayHint.refreshSupport", void 0);
    })(InlayHintRefreshRequest || (exports2.InlayHintRefreshRequest = InlayHintRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.diagnostic.js
var require_protocol_diagnostic = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.diagnostic.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DiagnosticRefreshRequest = exports2.WorkspaceDiagnosticRequest = exports2.DocumentDiagnosticRequest = exports2.DocumentDiagnosticReportKind = exports2.DiagnosticServerCancellationData = void 0;
    var vscode_jsonrpc_1 = require_api();
    var Is2 = __importStar(require_is3());
    var messages_1 = require_messages2();
    var DiagnosticServerCancellationData;
    (function(DiagnosticServerCancellationData2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.boolean(candidate.retriggerRequest);
      }
      DiagnosticServerCancellationData2.is = is;
    })(DiagnosticServerCancellationData || (exports2.DiagnosticServerCancellationData = DiagnosticServerCancellationData = {}));
    var DocumentDiagnosticReportKind;
    (function(DocumentDiagnosticReportKind2) {
      DocumentDiagnosticReportKind2.Full = "full";
      DocumentDiagnosticReportKind2.Unchanged = "unchanged";
    })(DocumentDiagnosticReportKind || (exports2.DocumentDiagnosticReportKind = DocumentDiagnosticReportKind = {}));
    var DocumentDiagnosticRequest;
    (function(DocumentDiagnosticRequest2) {
      DocumentDiagnosticRequest2.method = "textDocument/diagnostic";
      DocumentDiagnosticRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentDiagnosticRequest2.type = new messages_1.ProtocolRequestType(DocumentDiagnosticRequest2.method);
      DocumentDiagnosticRequest2.partialResult = new vscode_jsonrpc_1.ProgressType();
      DocumentDiagnosticRequest2.capabilities = messages_1.CM.create("textDocument.diagnostic", "diagnosticProvider");
    })(DocumentDiagnosticRequest || (exports2.DocumentDiagnosticRequest = DocumentDiagnosticRequest = {}));
    var WorkspaceDiagnosticRequest;
    (function(WorkspaceDiagnosticRequest2) {
      WorkspaceDiagnosticRequest2.method = "workspace/diagnostic";
      WorkspaceDiagnosticRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WorkspaceDiagnosticRequest2.type = new messages_1.ProtocolRequestType(WorkspaceDiagnosticRequest2.method);
      WorkspaceDiagnosticRequest2.partialResult = new vscode_jsonrpc_1.ProgressType();
      WorkspaceDiagnosticRequest2.capabilities = messages_1.CM.create("workspace.diagnostics", "diagnosticProvider.workspaceDiagnostics");
    })(WorkspaceDiagnosticRequest || (exports2.WorkspaceDiagnosticRequest = WorkspaceDiagnosticRequest = {}));
    var DiagnosticRefreshRequest;
    (function(DiagnosticRefreshRequest2) {
      DiagnosticRefreshRequest2.method = `workspace/diagnostic/refresh`;
      DiagnosticRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      DiagnosticRefreshRequest2.type = new messages_1.ProtocolRequestType0(DiagnosticRefreshRequest2.method);
      DiagnosticRefreshRequest2.capabilities = messages_1.CM.create("workspace.diagnostics.refreshSupport", void 0);
    })(DiagnosticRefreshRequest || (exports2.DiagnosticRefreshRequest = DiagnosticRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.notebook.js
var require_protocol_notebook = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.notebook.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DidCloseNotebookDocumentNotification = exports2.DidSaveNotebookDocumentNotification = exports2.DidChangeNotebookDocumentNotification = exports2.NotebookCellArrayChange = exports2.DidOpenNotebookDocumentNotification = exports2.NotebookDocumentSyncRegistrationType = exports2.NotebookDocument = exports2.NotebookCell = exports2.ExecutionSummary = exports2.NotebookCellKind = void 0;
    var vscode_languageserver_types_1 = (init_main(), __toCommonJS(main_exports));
    var Is2 = __importStar(require_is3());
    var messages_1 = require_messages2();
    var NotebookCellKind;
    (function(NotebookCellKind2) {
      NotebookCellKind2.Markup = 1;
      NotebookCellKind2.Code = 2;
      function is(value) {
        return value === 1 || value === 2;
      }
      NotebookCellKind2.is = is;
    })(NotebookCellKind || (exports2.NotebookCellKind = NotebookCellKind = {}));
    var ExecutionSummary;
    (function(ExecutionSummary2) {
      function create(executionOrder, success) {
        const result = { executionOrder };
        if (success === true || success === false) {
          result.success = success;
        }
        return result;
      }
      ExecutionSummary2.create = create;
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && vscode_languageserver_types_1.uinteger.is(candidate.executionOrder) && (candidate.success === void 0 || Is2.boolean(candidate.success));
      }
      ExecutionSummary2.is = is;
      function equals(one, other) {
        if (one === other) {
          return true;
        }
        if (one === null || one === void 0 || other === null || other === void 0) {
          return false;
        }
        return one.executionOrder === other.executionOrder && one.success === other.success;
      }
      ExecutionSummary2.equals = equals;
    })(ExecutionSummary || (exports2.ExecutionSummary = ExecutionSummary = {}));
    var NotebookCell;
    (function(NotebookCell2) {
      function create(kind, document) {
        return { kind, document };
      }
      NotebookCell2.create = create;
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && NotebookCellKind.is(candidate.kind) && vscode_languageserver_types_1.DocumentUri.is(candidate.document) && (candidate.metadata === void 0 || Is2.objectLiteral(candidate.metadata));
      }
      NotebookCell2.is = is;
      function diff(one, two) {
        const result = /* @__PURE__ */ new Set();
        if (one.document !== two.document) {
          result.add("document");
        }
        if (one.kind !== two.kind) {
          result.add("kind");
        }
        if (one.executionSummary !== two.executionSummary) {
          result.add("executionSummary");
        }
        if ((one.metadata !== void 0 || two.metadata !== void 0) && !equalsMetadata(one.metadata, two.metadata)) {
          result.add("metadata");
        }
        if ((one.executionSummary !== void 0 || two.executionSummary !== void 0) && !ExecutionSummary.equals(one.executionSummary, two.executionSummary)) {
          result.add("executionSummary");
        }
        return result;
      }
      NotebookCell2.diff = diff;
      function equalsMetadata(one, other) {
        if (one === other) {
          return true;
        }
        if (one === null || one === void 0 || other === null || other === void 0) {
          return false;
        }
        if (typeof one !== typeof other) {
          return false;
        }
        if (typeof one !== "object") {
          return false;
        }
        const oneArray = Array.isArray(one);
        const otherArray = Array.isArray(other);
        if (oneArray !== otherArray) {
          return false;
        }
        if (oneArray && otherArray) {
          if (one.length !== other.length) {
            return false;
          }
          for (let i = 0; i < one.length; i++) {
            if (!equalsMetadata(one[i], other[i])) {
              return false;
            }
          }
        }
        if (Is2.objectLiteral(one) && Is2.objectLiteral(other)) {
          const oneKeys = Object.keys(one);
          const otherKeys = Object.keys(other);
          if (oneKeys.length !== otherKeys.length) {
            return false;
          }
          oneKeys.sort();
          otherKeys.sort();
          if (!equalsMetadata(oneKeys, otherKeys)) {
            return false;
          }
          for (let i = 0; i < oneKeys.length; i++) {
            const prop = oneKeys[i];
            if (!equalsMetadata(one[prop], other[prop])) {
              return false;
            }
          }
        }
        return true;
      }
    })(NotebookCell || (exports2.NotebookCell = NotebookCell = {}));
    var NotebookDocument;
    (function(NotebookDocument2) {
      function create(uri, notebookType, version, cells) {
        return { uri, notebookType, version, cells };
      }
      NotebookDocument2.create = create;
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && Is2.string(candidate.uri) && vscode_languageserver_types_1.integer.is(candidate.version) && Is2.typedArray(candidate.cells, NotebookCell.is);
      }
      NotebookDocument2.is = is;
    })(NotebookDocument || (exports2.NotebookDocument = NotebookDocument = {}));
    var NotebookDocumentSyncRegistrationType;
    (function(NotebookDocumentSyncRegistrationType2) {
      NotebookDocumentSyncRegistrationType2.method = "notebookDocument/sync";
      NotebookDocumentSyncRegistrationType2.messageDirection = messages_1.MessageDirection.clientToServer;
      NotebookDocumentSyncRegistrationType2.type = new messages_1.RegistrationType(NotebookDocumentSyncRegistrationType2.method);
    })(NotebookDocumentSyncRegistrationType || (exports2.NotebookDocumentSyncRegistrationType = NotebookDocumentSyncRegistrationType = {}));
    var DidOpenNotebookDocumentNotification;
    (function(DidOpenNotebookDocumentNotification2) {
      DidOpenNotebookDocumentNotification2.method = "notebookDocument/didOpen";
      DidOpenNotebookDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidOpenNotebookDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidOpenNotebookDocumentNotification2.method);
      DidOpenNotebookDocumentNotification2.registrationMethod = NotebookDocumentSyncRegistrationType.method;
    })(DidOpenNotebookDocumentNotification || (exports2.DidOpenNotebookDocumentNotification = DidOpenNotebookDocumentNotification = {}));
    var NotebookCellArrayChange;
    (function(NotebookCellArrayChange2) {
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && vscode_languageserver_types_1.uinteger.is(candidate.start) && vscode_languageserver_types_1.uinteger.is(candidate.deleteCount) && (candidate.cells === void 0 || Is2.typedArray(candidate.cells, NotebookCell.is));
      }
      NotebookCellArrayChange2.is = is;
      function create(start, deleteCount, cells) {
        const result = { start, deleteCount };
        if (cells !== void 0) {
          result.cells = cells;
        }
        return result;
      }
      NotebookCellArrayChange2.create = create;
    })(NotebookCellArrayChange || (exports2.NotebookCellArrayChange = NotebookCellArrayChange = {}));
    var DidChangeNotebookDocumentNotification;
    (function(DidChangeNotebookDocumentNotification2) {
      DidChangeNotebookDocumentNotification2.method = "notebookDocument/didChange";
      DidChangeNotebookDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidChangeNotebookDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidChangeNotebookDocumentNotification2.method);
      DidChangeNotebookDocumentNotification2.registrationMethod = NotebookDocumentSyncRegistrationType.method;
    })(DidChangeNotebookDocumentNotification || (exports2.DidChangeNotebookDocumentNotification = DidChangeNotebookDocumentNotification = {}));
    var DidSaveNotebookDocumentNotification;
    (function(DidSaveNotebookDocumentNotification2) {
      DidSaveNotebookDocumentNotification2.method = "notebookDocument/didSave";
      DidSaveNotebookDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidSaveNotebookDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidSaveNotebookDocumentNotification2.method);
      DidSaveNotebookDocumentNotification2.registrationMethod = NotebookDocumentSyncRegistrationType.method;
    })(DidSaveNotebookDocumentNotification || (exports2.DidSaveNotebookDocumentNotification = DidSaveNotebookDocumentNotification = {}));
    var DidCloseNotebookDocumentNotification;
    (function(DidCloseNotebookDocumentNotification2) {
      DidCloseNotebookDocumentNotification2.method = "notebookDocument/didClose";
      DidCloseNotebookDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidCloseNotebookDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidCloseNotebookDocumentNotification2.method);
      DidCloseNotebookDocumentNotification2.registrationMethod = NotebookDocumentSyncRegistrationType.method;
    })(DidCloseNotebookDocumentNotification || (exports2.DidCloseNotebookDocumentNotification = DidCloseNotebookDocumentNotification = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.inlineCompletion.js
var require_protocol_inlineCompletion = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.inlineCompletion.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlineCompletionRequest = void 0;
    var messages_1 = require_messages2();
    var InlineCompletionRequest;
    (function(InlineCompletionRequest2) {
      InlineCompletionRequest2.method = "textDocument/inlineCompletion";
      InlineCompletionRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      InlineCompletionRequest2.type = new messages_1.ProtocolRequestType(InlineCompletionRequest2.method);
      InlineCompletionRequest2.capabilities = messages_1.CM.create("textDocument.inlineCompletion", "inlineCompletionProvider");
    })(InlineCompletionRequest || (exports2.InlineCompletionRequest = InlineCompletionRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.textDocumentContent.js
var require_protocol_textDocumentContent = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.textDocumentContent.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TextDocumentContentRefreshRequest = exports2.TextDocumentContentRequest = void 0;
    var messages_1 = require_messages2();
    var TextDocumentContentRequest;
    (function(TextDocumentContentRequest2) {
      TextDocumentContentRequest2.method = "workspace/textDocumentContent";
      TextDocumentContentRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      TextDocumentContentRequest2.type = new messages_1.ProtocolRequestType(TextDocumentContentRequest2.method);
      TextDocumentContentRequest2.capabilities = messages_1.CM.create("workspace.textDocumentContent", "workspace.textDocumentContent");
    })(TextDocumentContentRequest || (exports2.TextDocumentContentRequest = TextDocumentContentRequest = {}));
    var TextDocumentContentRefreshRequest;
    (function(TextDocumentContentRefreshRequest2) {
      TextDocumentContentRefreshRequest2.method = `workspace/textDocumentContent/refresh`;
      TextDocumentContentRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      TextDocumentContentRefreshRequest2.type = new messages_1.ProtocolRequestType(TextDocumentContentRefreshRequest2.method);
    })(TextDocumentContentRefreshRequest || (exports2.TextDocumentContentRefreshRequest = TextDocumentContentRefreshRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/protocol.js
var require_protocol = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/protocol.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CodeActionRequest = exports2.DocumentSymbolRequest = exports2.DocumentHighlightRequest = exports2.ReferencesRequest = exports2.DefinitionRequest = exports2.SignatureHelpRequest = exports2.SignatureHelpTriggerKind = exports2.HoverRequest = exports2.CompletionResolveRequest = exports2.CompletionRequest = exports2.CompletionTriggerKind = exports2.PublishDiagnosticsNotification = exports2.WatchKind = exports2.GlobPattern = exports2.RelativePattern = exports2.FileChangeType = exports2.DidChangeWatchedFilesNotification = exports2.WillSaveTextDocumentWaitUntilRequest = exports2.WillSaveTextDocumentNotification = exports2.TextDocumentSaveReason = exports2.DidSaveTextDocumentNotification = exports2.DidCloseTextDocumentNotification = exports2.DidChangeTextDocumentNotification = exports2.TextDocumentContentChangeEvent = exports2.DidOpenTextDocumentNotification = exports2.TextDocumentSyncKind = exports2.TelemetryEventNotification = exports2.LogMessageNotification = exports2.ShowMessageRequest = exports2.ShowMessageNotification = exports2.MessageType = exports2.DidChangeConfigurationNotification = exports2.ExitNotification = exports2.ShutdownRequest = exports2.InitializedNotification = exports2.InitializeErrorCodes = exports2.InitializeRequest = exports2.WorkDoneProgressOptions = exports2.TextDocumentRegistrationOptions = exports2.StaticRegistrationOptions = exports2.PositionEncodingKind = exports2.RegularExpressionEngineKind = exports2.FailureHandlingKind = exports2.ResourceOperationKind = exports2.UnregistrationRequest = exports2.RegistrationRequest = exports2.DocumentSelector = exports2.NotebookCellTextDocumentFilter = exports2.NotebookDocumentFilter = exports2.TextDocumentFilter = void 0;
    exports2.UniquenessLevel = exports2.WillDeleteFilesRequest = exports2.DidDeleteFilesNotification = exports2.WillRenameFilesRequest = exports2.DidRenameFilesNotification = exports2.WillCreateFilesRequest = exports2.DidCreateFilesNotification = exports2.FileOperationPatternKind = exports2.LinkedEditingRangeRequest = exports2.ShowDocumentRequest = exports2.SemanticTokensRegistrationType = exports2.SemanticTokensRefreshRequest = exports2.SemanticTokensRangeRequest = exports2.SemanticTokensDeltaRequest = exports2.SemanticTokensRequest = exports2.TokenFormat = exports2.CallHierarchyPrepareRequest = exports2.CallHierarchyOutgoingCallsRequest = exports2.CallHierarchyIncomingCallsRequest = exports2.WorkDoneProgressCancelNotification = exports2.WorkDoneProgressCreateRequest = exports2.WorkDoneProgress = exports2.SelectionRangeRequest = exports2.DeclarationRequest = exports2.FoldingRangeRefreshRequest = exports2.FoldingRangeRequest = exports2.ColorPresentationRequest = exports2.DocumentColorRequest = exports2.ConfigurationRequest = exports2.DidChangeWorkspaceFoldersNotification = exports2.WorkspaceFoldersRequest = exports2.TypeDefinitionRequest = exports2.ImplementationRequest = exports2.ApplyWorkspaceEditRequest = exports2.ExecuteCommandRequest = exports2.PrepareRenameRequest = exports2.RenameRequest = exports2.PrepareSupportDefaultBehavior = exports2.DocumentOnTypeFormattingRequest = exports2.DocumentRangesFormattingRequest = exports2.DocumentRangeFormattingRequest = exports2.DocumentFormattingRequest = exports2.DocumentLinkResolveRequest = exports2.DocumentLinkRequest = exports2.CodeLensRefreshRequest = exports2.CodeLensResolveRequest = exports2.CodeLensRequest = exports2.WorkspaceSymbolResolveRequest = exports2.WorkspaceSymbolRequest = exports2.CodeActionResolveRequest = void 0;
    exports2.TextDocumentContentRefreshRequest = exports2.TextDocumentContentRequest = exports2.InlineCompletionRequest = exports2.DidCloseNotebookDocumentNotification = exports2.DidSaveNotebookDocumentNotification = exports2.DidChangeNotebookDocumentNotification = exports2.NotebookCellArrayChange = exports2.DidOpenNotebookDocumentNotification = exports2.NotebookDocumentSyncRegistrationType = exports2.NotebookDocument = exports2.NotebookCell = exports2.ExecutionSummary = exports2.NotebookCellKind = exports2.DiagnosticRefreshRequest = exports2.WorkspaceDiagnosticRequest = exports2.DocumentDiagnosticRequest = exports2.DocumentDiagnosticReportKind = exports2.DiagnosticServerCancellationData = exports2.InlayHintRefreshRequest = exports2.InlayHintResolveRequest = exports2.InlayHintRequest = exports2.InlineValueRefreshRequest = exports2.InlineValueRequest = exports2.TypeHierarchySupertypesRequest = exports2.TypeHierarchySubtypesRequest = exports2.TypeHierarchyPrepareRequest = exports2.MonikerRequest = exports2.MonikerKind = void 0;
    var messages_1 = require_messages2();
    var vscode_languageserver_types_1 = (init_main(), __toCommonJS(main_exports));
    var Is2 = __importStar(require_is3());
    var protocol_implementation_1 = require_protocol_implementation();
    Object.defineProperty(exports2, "ImplementationRequest", { enumerable: true, get: function() {
      return protocol_implementation_1.ImplementationRequest;
    } });
    var protocol_typeDefinition_1 = require_protocol_typeDefinition();
    Object.defineProperty(exports2, "TypeDefinitionRequest", { enumerable: true, get: function() {
      return protocol_typeDefinition_1.TypeDefinitionRequest;
    } });
    var protocol_workspaceFolder_1 = require_protocol_workspaceFolder();
    Object.defineProperty(exports2, "WorkspaceFoldersRequest", { enumerable: true, get: function() {
      return protocol_workspaceFolder_1.WorkspaceFoldersRequest;
    } });
    Object.defineProperty(exports2, "DidChangeWorkspaceFoldersNotification", { enumerable: true, get: function() {
      return protocol_workspaceFolder_1.DidChangeWorkspaceFoldersNotification;
    } });
    var protocol_configuration_1 = require_protocol_configuration();
    Object.defineProperty(exports2, "ConfigurationRequest", { enumerable: true, get: function() {
      return protocol_configuration_1.ConfigurationRequest;
    } });
    var protocol_colorProvider_1 = require_protocol_colorProvider();
    Object.defineProperty(exports2, "DocumentColorRequest", { enumerable: true, get: function() {
      return protocol_colorProvider_1.DocumentColorRequest;
    } });
    Object.defineProperty(exports2, "ColorPresentationRequest", { enumerable: true, get: function() {
      return protocol_colorProvider_1.ColorPresentationRequest;
    } });
    var protocol_foldingRange_1 = require_protocol_foldingRange();
    Object.defineProperty(exports2, "FoldingRangeRequest", { enumerable: true, get: function() {
      return protocol_foldingRange_1.FoldingRangeRequest;
    } });
    Object.defineProperty(exports2, "FoldingRangeRefreshRequest", { enumerable: true, get: function() {
      return protocol_foldingRange_1.FoldingRangeRefreshRequest;
    } });
    var protocol_declaration_1 = require_protocol_declaration();
    Object.defineProperty(exports2, "DeclarationRequest", { enumerable: true, get: function() {
      return protocol_declaration_1.DeclarationRequest;
    } });
    var protocol_selectionRange_1 = require_protocol_selectionRange();
    Object.defineProperty(exports2, "SelectionRangeRequest", { enumerable: true, get: function() {
      return protocol_selectionRange_1.SelectionRangeRequest;
    } });
    var protocol_progress_1 = require_protocol_progress();
    Object.defineProperty(exports2, "WorkDoneProgress", { enumerable: true, get: function() {
      return protocol_progress_1.WorkDoneProgress;
    } });
    Object.defineProperty(exports2, "WorkDoneProgressCreateRequest", { enumerable: true, get: function() {
      return protocol_progress_1.WorkDoneProgressCreateRequest;
    } });
    Object.defineProperty(exports2, "WorkDoneProgressCancelNotification", { enumerable: true, get: function() {
      return protocol_progress_1.WorkDoneProgressCancelNotification;
    } });
    var protocol_callHierarchy_1 = require_protocol_callHierarchy();
    Object.defineProperty(exports2, "CallHierarchyIncomingCallsRequest", { enumerable: true, get: function() {
      return protocol_callHierarchy_1.CallHierarchyIncomingCallsRequest;
    } });
    Object.defineProperty(exports2, "CallHierarchyOutgoingCallsRequest", { enumerable: true, get: function() {
      return protocol_callHierarchy_1.CallHierarchyOutgoingCallsRequest;
    } });
    Object.defineProperty(exports2, "CallHierarchyPrepareRequest", { enumerable: true, get: function() {
      return protocol_callHierarchy_1.CallHierarchyPrepareRequest;
    } });
    var protocol_semanticTokens_1 = require_protocol_semanticTokens();
    Object.defineProperty(exports2, "TokenFormat", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.TokenFormat;
    } });
    Object.defineProperty(exports2, "SemanticTokensRequest", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.SemanticTokensRequest;
    } });
    Object.defineProperty(exports2, "SemanticTokensDeltaRequest", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.SemanticTokensDeltaRequest;
    } });
    Object.defineProperty(exports2, "SemanticTokensRangeRequest", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.SemanticTokensRangeRequest;
    } });
    Object.defineProperty(exports2, "SemanticTokensRefreshRequest", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.SemanticTokensRefreshRequest;
    } });
    Object.defineProperty(exports2, "SemanticTokensRegistrationType", { enumerable: true, get: function() {
      return protocol_semanticTokens_1.SemanticTokensRegistrationType;
    } });
    var protocol_showDocument_1 = require_protocol_showDocument();
    Object.defineProperty(exports2, "ShowDocumentRequest", { enumerable: true, get: function() {
      return protocol_showDocument_1.ShowDocumentRequest;
    } });
    var protocol_linkedEditingRange_1 = require_protocol_linkedEditingRange();
    Object.defineProperty(exports2, "LinkedEditingRangeRequest", { enumerable: true, get: function() {
      return protocol_linkedEditingRange_1.LinkedEditingRangeRequest;
    } });
    var protocol_fileOperations_1 = require_protocol_fileOperations();
    Object.defineProperty(exports2, "FileOperationPatternKind", { enumerable: true, get: function() {
      return protocol_fileOperations_1.FileOperationPatternKind;
    } });
    Object.defineProperty(exports2, "DidCreateFilesNotification", { enumerable: true, get: function() {
      return protocol_fileOperations_1.DidCreateFilesNotification;
    } });
    Object.defineProperty(exports2, "WillCreateFilesRequest", { enumerable: true, get: function() {
      return protocol_fileOperations_1.WillCreateFilesRequest;
    } });
    Object.defineProperty(exports2, "DidRenameFilesNotification", { enumerable: true, get: function() {
      return protocol_fileOperations_1.DidRenameFilesNotification;
    } });
    Object.defineProperty(exports2, "WillRenameFilesRequest", { enumerable: true, get: function() {
      return protocol_fileOperations_1.WillRenameFilesRequest;
    } });
    Object.defineProperty(exports2, "DidDeleteFilesNotification", { enumerable: true, get: function() {
      return protocol_fileOperations_1.DidDeleteFilesNotification;
    } });
    Object.defineProperty(exports2, "WillDeleteFilesRequest", { enumerable: true, get: function() {
      return protocol_fileOperations_1.WillDeleteFilesRequest;
    } });
    var protocol_moniker_1 = require_protocol_moniker();
    Object.defineProperty(exports2, "UniquenessLevel", { enumerable: true, get: function() {
      return protocol_moniker_1.UniquenessLevel;
    } });
    Object.defineProperty(exports2, "MonikerKind", { enumerable: true, get: function() {
      return protocol_moniker_1.MonikerKind;
    } });
    Object.defineProperty(exports2, "MonikerRequest", { enumerable: true, get: function() {
      return protocol_moniker_1.MonikerRequest;
    } });
    var protocol_typeHierarchy_1 = require_protocol_typeHierarchy();
    Object.defineProperty(exports2, "TypeHierarchyPrepareRequest", { enumerable: true, get: function() {
      return protocol_typeHierarchy_1.TypeHierarchyPrepareRequest;
    } });
    Object.defineProperty(exports2, "TypeHierarchySubtypesRequest", { enumerable: true, get: function() {
      return protocol_typeHierarchy_1.TypeHierarchySubtypesRequest;
    } });
    Object.defineProperty(exports2, "TypeHierarchySupertypesRequest", { enumerable: true, get: function() {
      return protocol_typeHierarchy_1.TypeHierarchySupertypesRequest;
    } });
    var protocol_inlineValue_1 = require_protocol_inlineValue();
    Object.defineProperty(exports2, "InlineValueRequest", { enumerable: true, get: function() {
      return protocol_inlineValue_1.InlineValueRequest;
    } });
    Object.defineProperty(exports2, "InlineValueRefreshRequest", { enumerable: true, get: function() {
      return protocol_inlineValue_1.InlineValueRefreshRequest;
    } });
    var protocol_inlayHint_1 = require_protocol_inlayHint();
    Object.defineProperty(exports2, "InlayHintRequest", { enumerable: true, get: function() {
      return protocol_inlayHint_1.InlayHintRequest;
    } });
    Object.defineProperty(exports2, "InlayHintResolveRequest", { enumerable: true, get: function() {
      return protocol_inlayHint_1.InlayHintResolveRequest;
    } });
    Object.defineProperty(exports2, "InlayHintRefreshRequest", { enumerable: true, get: function() {
      return protocol_inlayHint_1.InlayHintRefreshRequest;
    } });
    var protocol_diagnostic_1 = require_protocol_diagnostic();
    Object.defineProperty(exports2, "DiagnosticServerCancellationData", { enumerable: true, get: function() {
      return protocol_diagnostic_1.DiagnosticServerCancellationData;
    } });
    Object.defineProperty(exports2, "DocumentDiagnosticReportKind", { enumerable: true, get: function() {
      return protocol_diagnostic_1.DocumentDiagnosticReportKind;
    } });
    Object.defineProperty(exports2, "DocumentDiagnosticRequest", { enumerable: true, get: function() {
      return protocol_diagnostic_1.DocumentDiagnosticRequest;
    } });
    Object.defineProperty(exports2, "WorkspaceDiagnosticRequest", { enumerable: true, get: function() {
      return protocol_diagnostic_1.WorkspaceDiagnosticRequest;
    } });
    Object.defineProperty(exports2, "DiagnosticRefreshRequest", { enumerable: true, get: function() {
      return protocol_diagnostic_1.DiagnosticRefreshRequest;
    } });
    var protocol_notebook_1 = require_protocol_notebook();
    Object.defineProperty(exports2, "NotebookCellKind", { enumerable: true, get: function() {
      return protocol_notebook_1.NotebookCellKind;
    } });
    Object.defineProperty(exports2, "ExecutionSummary", { enumerable: true, get: function() {
      return protocol_notebook_1.ExecutionSummary;
    } });
    Object.defineProperty(exports2, "NotebookCell", { enumerable: true, get: function() {
      return protocol_notebook_1.NotebookCell;
    } });
    Object.defineProperty(exports2, "NotebookDocument", { enumerable: true, get: function() {
      return protocol_notebook_1.NotebookDocument;
    } });
    Object.defineProperty(exports2, "NotebookDocumentSyncRegistrationType", { enumerable: true, get: function() {
      return protocol_notebook_1.NotebookDocumentSyncRegistrationType;
    } });
    Object.defineProperty(exports2, "DidOpenNotebookDocumentNotification", { enumerable: true, get: function() {
      return protocol_notebook_1.DidOpenNotebookDocumentNotification;
    } });
    Object.defineProperty(exports2, "NotebookCellArrayChange", { enumerable: true, get: function() {
      return protocol_notebook_1.NotebookCellArrayChange;
    } });
    Object.defineProperty(exports2, "DidChangeNotebookDocumentNotification", { enumerable: true, get: function() {
      return protocol_notebook_1.DidChangeNotebookDocumentNotification;
    } });
    Object.defineProperty(exports2, "DidSaveNotebookDocumentNotification", { enumerable: true, get: function() {
      return protocol_notebook_1.DidSaveNotebookDocumentNotification;
    } });
    Object.defineProperty(exports2, "DidCloseNotebookDocumentNotification", { enumerable: true, get: function() {
      return protocol_notebook_1.DidCloseNotebookDocumentNotification;
    } });
    var protocol_inlineCompletion_1 = require_protocol_inlineCompletion();
    Object.defineProperty(exports2, "InlineCompletionRequest", { enumerable: true, get: function() {
      return protocol_inlineCompletion_1.InlineCompletionRequest;
    } });
    var protocol_textDocumentContent_1 = require_protocol_textDocumentContent();
    Object.defineProperty(exports2, "TextDocumentContentRequest", { enumerable: true, get: function() {
      return protocol_textDocumentContent_1.TextDocumentContentRequest;
    } });
    Object.defineProperty(exports2, "TextDocumentContentRefreshRequest", { enumerable: true, get: function() {
      return protocol_textDocumentContent_1.TextDocumentContentRefreshRequest;
    } });
    var TextDocumentFilter;
    (function(TextDocumentFilter2) {
      function is(value) {
        const candidate = value;
        return Is2.string(candidate) || (Is2.string(candidate.language) || Is2.string(candidate.scheme) || GlobPattern.is(candidate.pattern));
      }
      TextDocumentFilter2.is = is;
    })(TextDocumentFilter || (exports2.TextDocumentFilter = TextDocumentFilter = {}));
    var NotebookDocumentFilter;
    (function(NotebookDocumentFilter2) {
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && (Is2.string(candidate.notebookType) || Is2.string(candidate.scheme) || Is2.string(candidate.pattern));
      }
      NotebookDocumentFilter2.is = is;
    })(NotebookDocumentFilter || (exports2.NotebookDocumentFilter = NotebookDocumentFilter = {}));
    var NotebookCellTextDocumentFilter;
    (function(NotebookCellTextDocumentFilter2) {
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && (Is2.string(candidate.notebook) || NotebookDocumentFilter.is(candidate.notebook)) && (candidate.language === void 0 || Is2.string(candidate.language));
      }
      NotebookCellTextDocumentFilter2.is = is;
    })(NotebookCellTextDocumentFilter || (exports2.NotebookCellTextDocumentFilter = NotebookCellTextDocumentFilter = {}));
    var DocumentSelector;
    (function(DocumentSelector2) {
      function is(value) {
        if (!Array.isArray(value)) {
          return false;
        }
        for (const elem of value) {
          if (!Is2.string(elem) && !TextDocumentFilter.is(elem) && !NotebookCellTextDocumentFilter.is(elem)) {
            return false;
          }
        }
        return true;
      }
      DocumentSelector2.is = is;
    })(DocumentSelector || (exports2.DocumentSelector = DocumentSelector = {}));
    var RegistrationRequest;
    (function(RegistrationRequest2) {
      RegistrationRequest2.method = "client/registerCapability";
      RegistrationRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      RegistrationRequest2.type = new messages_1.ProtocolRequestType(RegistrationRequest2.method);
    })(RegistrationRequest || (exports2.RegistrationRequest = RegistrationRequest = {}));
    var UnregistrationRequest;
    (function(UnregistrationRequest2) {
      UnregistrationRequest2.method = "client/unregisterCapability";
      UnregistrationRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      UnregistrationRequest2.type = new messages_1.ProtocolRequestType(UnregistrationRequest2.method);
    })(UnregistrationRequest || (exports2.UnregistrationRequest = UnregistrationRequest = {}));
    var ResourceOperationKind;
    (function(ResourceOperationKind2) {
      ResourceOperationKind2.Create = "create";
      ResourceOperationKind2.Rename = "rename";
      ResourceOperationKind2.Delete = "delete";
    })(ResourceOperationKind || (exports2.ResourceOperationKind = ResourceOperationKind = {}));
    var FailureHandlingKind;
    (function(FailureHandlingKind2) {
      FailureHandlingKind2.Abort = "abort";
      FailureHandlingKind2.Transactional = "transactional";
      FailureHandlingKind2.TextOnlyTransactional = "textOnlyTransactional";
      FailureHandlingKind2.Undo = "undo";
    })(FailureHandlingKind || (exports2.FailureHandlingKind = FailureHandlingKind = {}));
    var RegularExpressionEngineKind;
    (function(RegularExpressionEngineKind2) {
      RegularExpressionEngineKind2.ES2020 = "ES2020";
    })(RegularExpressionEngineKind || (exports2.RegularExpressionEngineKind = RegularExpressionEngineKind = {}));
    var PositionEncodingKind;
    (function(PositionEncodingKind2) {
      PositionEncodingKind2.UTF8 = "utf-8";
      PositionEncodingKind2.UTF16 = "utf-16";
      PositionEncodingKind2.UTF32 = "utf-32";
    })(PositionEncodingKind || (exports2.PositionEncodingKind = PositionEncodingKind = {}));
    var StaticRegistrationOptions;
    (function(StaticRegistrationOptions2) {
      function hasId(value) {
        const candidate = value;
        return candidate && Is2.string(candidate.id) && candidate.id.length > 0;
      }
      StaticRegistrationOptions2.hasId = hasId;
    })(StaticRegistrationOptions || (exports2.StaticRegistrationOptions = StaticRegistrationOptions = {}));
    var TextDocumentRegistrationOptions;
    (function(TextDocumentRegistrationOptions2) {
      function is(value) {
        const candidate = value;
        return candidate && (candidate.documentSelector === null || DocumentSelector.is(candidate.documentSelector));
      }
      TextDocumentRegistrationOptions2.is = is;
    })(TextDocumentRegistrationOptions || (exports2.TextDocumentRegistrationOptions = TextDocumentRegistrationOptions = {}));
    var WorkDoneProgressOptions;
    (function(WorkDoneProgressOptions2) {
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && (candidate.workDoneProgress === void 0 || Is2.boolean(candidate.workDoneProgress));
      }
      WorkDoneProgressOptions2.is = is;
      function hasWorkDoneProgress(value) {
        const candidate = value;
        return candidate && Is2.boolean(candidate.workDoneProgress);
      }
      WorkDoneProgressOptions2.hasWorkDoneProgress = hasWorkDoneProgress;
    })(WorkDoneProgressOptions || (exports2.WorkDoneProgressOptions = WorkDoneProgressOptions = {}));
    var InitializeRequest;
    (function(InitializeRequest2) {
      InitializeRequest2.method = "initialize";
      InitializeRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      InitializeRequest2.type = new messages_1.ProtocolRequestType(InitializeRequest2.method);
    })(InitializeRequest || (exports2.InitializeRequest = InitializeRequest = {}));
    var InitializeErrorCodes;
    (function(InitializeErrorCodes2) {
      InitializeErrorCodes2.unknownProtocolVersion = 1;
    })(InitializeErrorCodes || (exports2.InitializeErrorCodes = InitializeErrorCodes = {}));
    var InitializedNotification;
    (function(InitializedNotification2) {
      InitializedNotification2.method = "initialized";
      InitializedNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      InitializedNotification2.type = new messages_1.ProtocolNotificationType(InitializedNotification2.method);
    })(InitializedNotification || (exports2.InitializedNotification = InitializedNotification = {}));
    var ShutdownRequest;
    (function(ShutdownRequest2) {
      ShutdownRequest2.method = "shutdown";
      ShutdownRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      ShutdownRequest2.type = new messages_1.ProtocolRequestType0(ShutdownRequest2.method);
    })(ShutdownRequest || (exports2.ShutdownRequest = ShutdownRequest = {}));
    var ExitNotification;
    (function(ExitNotification2) {
      ExitNotification2.method = "exit";
      ExitNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      ExitNotification2.type = new messages_1.ProtocolNotificationType0(ExitNotification2.method);
    })(ExitNotification || (exports2.ExitNotification = ExitNotification = {}));
    var DidChangeConfigurationNotification;
    (function(DidChangeConfigurationNotification2) {
      DidChangeConfigurationNotification2.method = "workspace/didChangeConfiguration";
      DidChangeConfigurationNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidChangeConfigurationNotification2.type = new messages_1.ProtocolNotificationType(DidChangeConfigurationNotification2.method);
      DidChangeConfigurationNotification2.capabilities = messages_1.CM.create("workspace.didChangeConfiguration", void 0);
    })(DidChangeConfigurationNotification || (exports2.DidChangeConfigurationNotification = DidChangeConfigurationNotification = {}));
    var MessageType;
    (function(MessageType2) {
      MessageType2.Error = 1;
      MessageType2.Warning = 2;
      MessageType2.Info = 3;
      MessageType2.Log = 4;
      MessageType2.Debug = 5;
    })(MessageType || (exports2.MessageType = MessageType = {}));
    var ShowMessageNotification;
    (function(ShowMessageNotification2) {
      ShowMessageNotification2.method = "window/showMessage";
      ShowMessageNotification2.messageDirection = messages_1.MessageDirection.serverToClient;
      ShowMessageNotification2.type = new messages_1.ProtocolNotificationType(ShowMessageNotification2.method);
      ShowMessageNotification2.capabilities = messages_1.CM.create("window.showMessage", void 0);
    })(ShowMessageNotification || (exports2.ShowMessageNotification = ShowMessageNotification = {}));
    var ShowMessageRequest;
    (function(ShowMessageRequest2) {
      ShowMessageRequest2.method = "window/showMessageRequest";
      ShowMessageRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      ShowMessageRequest2.type = new messages_1.ProtocolRequestType(ShowMessageRequest2.method);
      ShowMessageRequest2.capabilities = messages_1.CM.create("window.showMessage", void 0);
    })(ShowMessageRequest || (exports2.ShowMessageRequest = ShowMessageRequest = {}));
    var LogMessageNotification;
    (function(LogMessageNotification2) {
      LogMessageNotification2.method = "window/logMessage";
      LogMessageNotification2.messageDirection = messages_1.MessageDirection.serverToClient;
      LogMessageNotification2.type = new messages_1.ProtocolNotificationType(LogMessageNotification2.method);
    })(LogMessageNotification || (exports2.LogMessageNotification = LogMessageNotification = {}));
    var TelemetryEventNotification;
    (function(TelemetryEventNotification2) {
      TelemetryEventNotification2.method = "telemetry/event";
      TelemetryEventNotification2.messageDirection = messages_1.MessageDirection.serverToClient;
      TelemetryEventNotification2.type = new messages_1.ProtocolNotificationType(TelemetryEventNotification2.method);
    })(TelemetryEventNotification || (exports2.TelemetryEventNotification = TelemetryEventNotification = {}));
    var TextDocumentSyncKind;
    (function(TextDocumentSyncKind2) {
      TextDocumentSyncKind2.None = 0;
      TextDocumentSyncKind2.Full = 1;
      TextDocumentSyncKind2.Incremental = 2;
    })(TextDocumentSyncKind || (exports2.TextDocumentSyncKind = TextDocumentSyncKind = {}));
    var DidOpenTextDocumentNotification;
    (function(DidOpenTextDocumentNotification2) {
      DidOpenTextDocumentNotification2.method = "textDocument/didOpen";
      DidOpenTextDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidOpenTextDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidOpenTextDocumentNotification2.method);
      DidOpenTextDocumentNotification2.capabilities = messages_1.CM.create("textDocument.synchronization", "textDocumentSync.openClose");
    })(DidOpenTextDocumentNotification || (exports2.DidOpenTextDocumentNotification = DidOpenTextDocumentNotification = {}));
    var TextDocumentContentChangeEvent;
    (function(TextDocumentContentChangeEvent2) {
      function isIncremental(event) {
        const candidate = event;
        return candidate !== void 0 && candidate !== null && typeof candidate.text === "string" && candidate.range !== void 0 && (candidate.rangeLength === void 0 || typeof candidate.rangeLength === "number");
      }
      TextDocumentContentChangeEvent2.isIncremental = isIncremental;
      function isFull(event) {
        const candidate = event;
        return candidate !== void 0 && candidate !== null && typeof candidate.text === "string" && candidate.range === void 0 && candidate.rangeLength === void 0;
      }
      TextDocumentContentChangeEvent2.isFull = isFull;
    })(TextDocumentContentChangeEvent || (exports2.TextDocumentContentChangeEvent = TextDocumentContentChangeEvent = {}));
    var DidChangeTextDocumentNotification;
    (function(DidChangeTextDocumentNotification2) {
      DidChangeTextDocumentNotification2.method = "textDocument/didChange";
      DidChangeTextDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidChangeTextDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidChangeTextDocumentNotification2.method);
      DidChangeTextDocumentNotification2.capabilities = messages_1.CM.create("textDocument.synchronization", "textDocumentSync");
    })(DidChangeTextDocumentNotification || (exports2.DidChangeTextDocumentNotification = DidChangeTextDocumentNotification = {}));
    var DidCloseTextDocumentNotification;
    (function(DidCloseTextDocumentNotification2) {
      DidCloseTextDocumentNotification2.method = "textDocument/didClose";
      DidCloseTextDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidCloseTextDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidCloseTextDocumentNotification2.method);
      DidCloseTextDocumentNotification2.capabilities = messages_1.CM.create("textDocument.synchronization", "textDocumentSync.openClose");
    })(DidCloseTextDocumentNotification || (exports2.DidCloseTextDocumentNotification = DidCloseTextDocumentNotification = {}));
    var DidSaveTextDocumentNotification;
    (function(DidSaveTextDocumentNotification2) {
      DidSaveTextDocumentNotification2.method = "textDocument/didSave";
      DidSaveTextDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidSaveTextDocumentNotification2.type = new messages_1.ProtocolNotificationType(DidSaveTextDocumentNotification2.method);
      DidSaveTextDocumentNotification2.capabilities = messages_1.CM.create("textDocument.synchronization.didSave", "textDocumentSync.save");
    })(DidSaveTextDocumentNotification || (exports2.DidSaveTextDocumentNotification = DidSaveTextDocumentNotification = {}));
    var TextDocumentSaveReason;
    (function(TextDocumentSaveReason2) {
      TextDocumentSaveReason2.Manual = 1;
      TextDocumentSaveReason2.AfterDelay = 2;
      TextDocumentSaveReason2.FocusOut = 3;
    })(TextDocumentSaveReason || (exports2.TextDocumentSaveReason = TextDocumentSaveReason = {}));
    var WillSaveTextDocumentNotification;
    (function(WillSaveTextDocumentNotification2) {
      WillSaveTextDocumentNotification2.method = "textDocument/willSave";
      WillSaveTextDocumentNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      WillSaveTextDocumentNotification2.type = new messages_1.ProtocolNotificationType(WillSaveTextDocumentNotification2.method);
      WillSaveTextDocumentNotification2.capabilities = messages_1.CM.create("textDocument.synchronization.willSave", "textDocumentSync.willSave");
    })(WillSaveTextDocumentNotification || (exports2.WillSaveTextDocumentNotification = WillSaveTextDocumentNotification = {}));
    var WillSaveTextDocumentWaitUntilRequest;
    (function(WillSaveTextDocumentWaitUntilRequest2) {
      WillSaveTextDocumentWaitUntilRequest2.method = "textDocument/willSaveWaitUntil";
      WillSaveTextDocumentWaitUntilRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WillSaveTextDocumentWaitUntilRequest2.type = new messages_1.ProtocolRequestType(WillSaveTextDocumentWaitUntilRequest2.method);
      WillSaveTextDocumentWaitUntilRequest2.capabilities = messages_1.CM.create("textDocument.synchronization.willSaveWaitUntil", "textDocumentSync.willSaveWaitUntil");
    })(WillSaveTextDocumentWaitUntilRequest || (exports2.WillSaveTextDocumentWaitUntilRequest = WillSaveTextDocumentWaitUntilRequest = {}));
    var DidChangeWatchedFilesNotification;
    (function(DidChangeWatchedFilesNotification2) {
      DidChangeWatchedFilesNotification2.method = "workspace/didChangeWatchedFiles";
      DidChangeWatchedFilesNotification2.messageDirection = messages_1.MessageDirection.clientToServer;
      DidChangeWatchedFilesNotification2.type = new messages_1.ProtocolNotificationType(DidChangeWatchedFilesNotification2.method);
      DidChangeWatchedFilesNotification2.capabilities = messages_1.CM.create("workspace.didChangeWatchedFiles", void 0);
    })(DidChangeWatchedFilesNotification || (exports2.DidChangeWatchedFilesNotification = DidChangeWatchedFilesNotification = {}));
    var FileChangeType;
    (function(FileChangeType2) {
      FileChangeType2.Created = 1;
      FileChangeType2.Changed = 2;
      FileChangeType2.Deleted = 3;
    })(FileChangeType || (exports2.FileChangeType = FileChangeType = {}));
    var RelativePattern;
    (function(RelativePattern2) {
      function is(value) {
        const candidate = value;
        return Is2.objectLiteral(candidate) && (vscode_languageserver_types_1.URI.is(candidate.baseUri) || vscode_languageserver_types_1.WorkspaceFolder.is(candidate.baseUri)) && Is2.string(candidate.pattern);
      }
      RelativePattern2.is = is;
    })(RelativePattern || (exports2.RelativePattern = RelativePattern = {}));
    var GlobPattern;
    (function(GlobPattern2) {
      function is(value) {
        const candidate = value;
        return Is2.string(candidate) || RelativePattern.is(candidate);
      }
      GlobPattern2.is = is;
    })(GlobPattern || (exports2.GlobPattern = GlobPattern = {}));
    var WatchKind;
    (function(WatchKind2) {
      WatchKind2.Create = 1;
      WatchKind2.Change = 2;
      WatchKind2.Delete = 4;
    })(WatchKind || (exports2.WatchKind = WatchKind = {}));
    var PublishDiagnosticsNotification;
    (function(PublishDiagnosticsNotification2) {
      PublishDiagnosticsNotification2.method = "textDocument/publishDiagnostics";
      PublishDiagnosticsNotification2.messageDirection = messages_1.MessageDirection.serverToClient;
      PublishDiagnosticsNotification2.type = new messages_1.ProtocolNotificationType(PublishDiagnosticsNotification2.method);
      PublishDiagnosticsNotification2.capabilities = messages_1.CM.create("textDocument.publishDiagnostics", void 0);
    })(PublishDiagnosticsNotification || (exports2.PublishDiagnosticsNotification = PublishDiagnosticsNotification = {}));
    var CompletionTriggerKind;
    (function(CompletionTriggerKind2) {
      CompletionTriggerKind2.Invoked = 1;
      CompletionTriggerKind2.TriggerCharacter = 2;
      CompletionTriggerKind2.TriggerForIncompleteCompletions = 3;
    })(CompletionTriggerKind || (exports2.CompletionTriggerKind = CompletionTriggerKind = {}));
    var CompletionRequest;
    (function(CompletionRequest2) {
      CompletionRequest2.method = "textDocument/completion";
      CompletionRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CompletionRequest2.type = new messages_1.ProtocolRequestType(CompletionRequest2.method);
      CompletionRequest2.capabilities = messages_1.CM.create("textDocument.completion", "completionProvider");
    })(CompletionRequest || (exports2.CompletionRequest = CompletionRequest = {}));
    var CompletionResolveRequest;
    (function(CompletionResolveRequest2) {
      CompletionResolveRequest2.method = "completionItem/resolve";
      CompletionResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CompletionResolveRequest2.type = new messages_1.ProtocolRequestType(CompletionResolveRequest2.method);
      CompletionResolveRequest2.capabilities = messages_1.CM.create("textDocument.completion.completionItem.resolveSupport", "completionProvider.resolveProvider");
    })(CompletionResolveRequest || (exports2.CompletionResolveRequest = CompletionResolveRequest = {}));
    var HoverRequest;
    (function(HoverRequest2) {
      HoverRequest2.method = "textDocument/hover";
      HoverRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      HoverRequest2.type = new messages_1.ProtocolRequestType(HoverRequest2.method);
      HoverRequest2.capabilities = messages_1.CM.create("textDocument.hover", "hoverProvider");
    })(HoverRequest || (exports2.HoverRequest = HoverRequest = {}));
    var SignatureHelpTriggerKind;
    (function(SignatureHelpTriggerKind2) {
      SignatureHelpTriggerKind2.Invoked = 1;
      SignatureHelpTriggerKind2.TriggerCharacter = 2;
      SignatureHelpTriggerKind2.ContentChange = 3;
    })(SignatureHelpTriggerKind || (exports2.SignatureHelpTriggerKind = SignatureHelpTriggerKind = {}));
    var SignatureHelpRequest;
    (function(SignatureHelpRequest2) {
      SignatureHelpRequest2.method = "textDocument/signatureHelp";
      SignatureHelpRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      SignatureHelpRequest2.type = new messages_1.ProtocolRequestType(SignatureHelpRequest2.method);
      SignatureHelpRequest2.capabilities = messages_1.CM.create("textDocument.signatureHelp", "signatureHelpProvider");
    })(SignatureHelpRequest || (exports2.SignatureHelpRequest = SignatureHelpRequest = {}));
    var DefinitionRequest;
    (function(DefinitionRequest2) {
      DefinitionRequest2.method = "textDocument/definition";
      DefinitionRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DefinitionRequest2.type = new messages_1.ProtocolRequestType(DefinitionRequest2.method);
      DefinitionRequest2.capabilities = messages_1.CM.create("textDocument.definition", "definitionProvider");
    })(DefinitionRequest || (exports2.DefinitionRequest = DefinitionRequest = {}));
    var ReferencesRequest;
    (function(ReferencesRequest2) {
      ReferencesRequest2.method = "textDocument/references";
      ReferencesRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      ReferencesRequest2.type = new messages_1.ProtocolRequestType(ReferencesRequest2.method);
      ReferencesRequest2.capabilities = messages_1.CM.create("textDocument.references", "referencesProvider");
    })(ReferencesRequest || (exports2.ReferencesRequest = ReferencesRequest = {}));
    var DocumentHighlightRequest;
    (function(DocumentHighlightRequest2) {
      DocumentHighlightRequest2.method = "textDocument/documentHighlight";
      DocumentHighlightRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentHighlightRequest2.type = new messages_1.ProtocolRequestType(DocumentHighlightRequest2.method);
      DocumentHighlightRequest2.capabilities = messages_1.CM.create("textDocument.documentHighlight", "documentHighlightProvider");
    })(DocumentHighlightRequest || (exports2.DocumentHighlightRequest = DocumentHighlightRequest = {}));
    var DocumentSymbolRequest;
    (function(DocumentSymbolRequest2) {
      DocumentSymbolRequest2.method = "textDocument/documentSymbol";
      DocumentSymbolRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentSymbolRequest2.type = new messages_1.ProtocolRequestType(DocumentSymbolRequest2.method);
      DocumentSymbolRequest2.capabilities = messages_1.CM.create("textDocument.documentSymbol", "documentSymbolProvider");
    })(DocumentSymbolRequest || (exports2.DocumentSymbolRequest = DocumentSymbolRequest = {}));
    var CodeActionRequest;
    (function(CodeActionRequest2) {
      CodeActionRequest2.method = "textDocument/codeAction";
      CodeActionRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CodeActionRequest2.type = new messages_1.ProtocolRequestType(CodeActionRequest2.method);
      CodeActionRequest2.capabilities = messages_1.CM.create("textDocument.codeAction", "codeActionProvider");
    })(CodeActionRequest || (exports2.CodeActionRequest = CodeActionRequest = {}));
    var CodeActionResolveRequest;
    (function(CodeActionResolveRequest2) {
      CodeActionResolveRequest2.method = "codeAction/resolve";
      CodeActionResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CodeActionResolveRequest2.type = new messages_1.ProtocolRequestType(CodeActionResolveRequest2.method);
      CodeActionResolveRequest2.capabilities = messages_1.CM.create("textDocument.codeAction.resolveSupport", "codeActionProvider.resolveProvider");
    })(CodeActionResolveRequest || (exports2.CodeActionResolveRequest = CodeActionResolveRequest = {}));
    var WorkspaceSymbolRequest;
    (function(WorkspaceSymbolRequest2) {
      WorkspaceSymbolRequest2.method = "workspace/symbol";
      WorkspaceSymbolRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WorkspaceSymbolRequest2.type = new messages_1.ProtocolRequestType(WorkspaceSymbolRequest2.method);
      WorkspaceSymbolRequest2.capabilities = messages_1.CM.create("workspace.symbol", "workspaceSymbolProvider");
    })(WorkspaceSymbolRequest || (exports2.WorkspaceSymbolRequest = WorkspaceSymbolRequest = {}));
    var WorkspaceSymbolResolveRequest;
    (function(WorkspaceSymbolResolveRequest2) {
      WorkspaceSymbolResolveRequest2.method = "workspaceSymbol/resolve";
      WorkspaceSymbolResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      WorkspaceSymbolResolveRequest2.type = new messages_1.ProtocolRequestType(WorkspaceSymbolResolveRequest2.method);
      WorkspaceSymbolResolveRequest2.capabilities = messages_1.CM.create("workspace.symbol.resolveSupport", "workspaceSymbolProvider.resolveProvider");
    })(WorkspaceSymbolResolveRequest || (exports2.WorkspaceSymbolResolveRequest = WorkspaceSymbolResolveRequest = {}));
    var CodeLensRequest;
    (function(CodeLensRequest2) {
      CodeLensRequest2.method = "textDocument/codeLens";
      CodeLensRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CodeLensRequest2.type = new messages_1.ProtocolRequestType(CodeLensRequest2.method);
      CodeLensRequest2.capabilities = messages_1.CM.create("textDocument.codeLens", "codeLensProvider");
    })(CodeLensRequest || (exports2.CodeLensRequest = CodeLensRequest = {}));
    var CodeLensResolveRequest;
    (function(CodeLensResolveRequest2) {
      CodeLensResolveRequest2.method = "codeLens/resolve";
      CodeLensResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      CodeLensResolveRequest2.type = new messages_1.ProtocolRequestType(CodeLensResolveRequest2.method);
      CodeLensResolveRequest2.capabilities = messages_1.CM.create("textDocument.codeLens.resolveSupport", "codeLensProvider.resolveProvider");
    })(CodeLensResolveRequest || (exports2.CodeLensResolveRequest = CodeLensResolveRequest = {}));
    var CodeLensRefreshRequest;
    (function(CodeLensRefreshRequest2) {
      CodeLensRefreshRequest2.method = `workspace/codeLens/refresh`;
      CodeLensRefreshRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      CodeLensRefreshRequest2.type = new messages_1.ProtocolRequestType0(CodeLensRefreshRequest2.method);
      CodeLensRefreshRequest2.capabilities = messages_1.CM.create("workspace.codeLens", void 0);
    })(CodeLensRefreshRequest || (exports2.CodeLensRefreshRequest = CodeLensRefreshRequest = {}));
    var DocumentLinkRequest;
    (function(DocumentLinkRequest2) {
      DocumentLinkRequest2.method = "textDocument/documentLink";
      DocumentLinkRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentLinkRequest2.type = new messages_1.ProtocolRequestType(DocumentLinkRequest2.method);
      DocumentLinkRequest2.capabilities = messages_1.CM.create("textDocument.documentLink", "documentLinkProvider");
    })(DocumentLinkRequest || (exports2.DocumentLinkRequest = DocumentLinkRequest = {}));
    var DocumentLinkResolveRequest;
    (function(DocumentLinkResolveRequest2) {
      DocumentLinkResolveRequest2.method = "documentLink/resolve";
      DocumentLinkResolveRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentLinkResolveRequest2.type = new messages_1.ProtocolRequestType(DocumentLinkResolveRequest2.method);
      DocumentLinkResolveRequest2.capabilities = messages_1.CM.create("textDocument.documentLink", "documentLinkProvider.resolveProvider");
    })(DocumentLinkResolveRequest || (exports2.DocumentLinkResolveRequest = DocumentLinkResolveRequest = {}));
    var DocumentFormattingRequest;
    (function(DocumentFormattingRequest2) {
      DocumentFormattingRequest2.method = "textDocument/formatting";
      DocumentFormattingRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentFormattingRequest2.type = new messages_1.ProtocolRequestType(DocumentFormattingRequest2.method);
      DocumentFormattingRequest2.capabilities = messages_1.CM.create("textDocument.formatting", "documentFormattingProvider");
    })(DocumentFormattingRequest || (exports2.DocumentFormattingRequest = DocumentFormattingRequest = {}));
    var DocumentRangeFormattingRequest;
    (function(DocumentRangeFormattingRequest2) {
      DocumentRangeFormattingRequest2.method = "textDocument/rangeFormatting";
      DocumentRangeFormattingRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentRangeFormattingRequest2.type = new messages_1.ProtocolRequestType(DocumentRangeFormattingRequest2.method);
      DocumentRangeFormattingRequest2.capabilities = messages_1.CM.create("textDocument.rangeFormatting", "documentRangeFormattingProvider");
    })(DocumentRangeFormattingRequest || (exports2.DocumentRangeFormattingRequest = DocumentRangeFormattingRequest = {}));
    var DocumentRangesFormattingRequest;
    (function(DocumentRangesFormattingRequest2) {
      DocumentRangesFormattingRequest2.method = "textDocument/rangesFormatting";
      DocumentRangesFormattingRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentRangesFormattingRequest2.type = new messages_1.ProtocolRequestType(DocumentRangesFormattingRequest2.method);
      DocumentRangesFormattingRequest2.capabilities = messages_1.CM.create("textDocument.rangeFormatting.rangesSupport", "documentRangeFormattingProvider.rangesSupport");
    })(DocumentRangesFormattingRequest || (exports2.DocumentRangesFormattingRequest = DocumentRangesFormattingRequest = {}));
    var DocumentOnTypeFormattingRequest;
    (function(DocumentOnTypeFormattingRequest2) {
      DocumentOnTypeFormattingRequest2.method = "textDocument/onTypeFormatting";
      DocumentOnTypeFormattingRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      DocumentOnTypeFormattingRequest2.type = new messages_1.ProtocolRequestType(DocumentOnTypeFormattingRequest2.method);
      DocumentOnTypeFormattingRequest2.capabilities = messages_1.CM.create("textDocument.onTypeFormatting", "documentOnTypeFormattingProvider");
    })(DocumentOnTypeFormattingRequest || (exports2.DocumentOnTypeFormattingRequest = DocumentOnTypeFormattingRequest = {}));
    var PrepareSupportDefaultBehavior;
    (function(PrepareSupportDefaultBehavior2) {
      PrepareSupportDefaultBehavior2.Identifier = 1;
    })(PrepareSupportDefaultBehavior || (exports2.PrepareSupportDefaultBehavior = PrepareSupportDefaultBehavior = {}));
    var RenameRequest;
    (function(RenameRequest2) {
      RenameRequest2.method = "textDocument/rename";
      RenameRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      RenameRequest2.type = new messages_1.ProtocolRequestType(RenameRequest2.method);
      RenameRequest2.capabilities = messages_1.CM.create("textDocument.rename", "renameProvider");
    })(RenameRequest || (exports2.RenameRequest = RenameRequest = {}));
    var PrepareRenameRequest;
    (function(PrepareRenameRequest2) {
      PrepareRenameRequest2.method = "textDocument/prepareRename";
      PrepareRenameRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      PrepareRenameRequest2.type = new messages_1.ProtocolRequestType(PrepareRenameRequest2.method);
      PrepareRenameRequest2.capabilities = messages_1.CM.create("textDocument.rename.prepareSupport", "renameProvider.prepareProvider");
    })(PrepareRenameRequest || (exports2.PrepareRenameRequest = PrepareRenameRequest = {}));
    var ExecuteCommandRequest;
    (function(ExecuteCommandRequest2) {
      ExecuteCommandRequest2.method = "workspace/executeCommand";
      ExecuteCommandRequest2.messageDirection = messages_1.MessageDirection.clientToServer;
      ExecuteCommandRequest2.type = new messages_1.ProtocolRequestType(ExecuteCommandRequest2.method);
      ExecuteCommandRequest2.capabilities = messages_1.CM.create("workspace.executeCommand", "executeCommandProvider");
    })(ExecuteCommandRequest || (exports2.ExecuteCommandRequest = ExecuteCommandRequest = {}));
    var ApplyWorkspaceEditRequest;
    (function(ApplyWorkspaceEditRequest2) {
      ApplyWorkspaceEditRequest2.method = "workspace/applyEdit";
      ApplyWorkspaceEditRequest2.messageDirection = messages_1.MessageDirection.serverToClient;
      ApplyWorkspaceEditRequest2.type = new messages_1.ProtocolRequestType("workspace/applyEdit");
      ApplyWorkspaceEditRequest2.capabilities = messages_1.CM.create("workspace.applyEdit", void 0);
    })(ApplyWorkspaceEditRequest || (exports2.ApplyWorkspaceEditRequest = ApplyWorkspaceEditRequest = {}));
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/connection.js
var require_connection2 = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/connection.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.createProtocolConnection = createProtocolConnection;
    var vscode_jsonrpc_1 = require_api();
    function createProtocolConnection(input, output, logger, options) {
      if (vscode_jsonrpc_1.ConnectionStrategy.is(options)) {
        options = { connectionStrategy: options };
      }
      return (0, vscode_jsonrpc_1.createMessageConnection)(input, output, logger, options);
    }
  }
});

// node_modules/vscode-languageserver-protocol/lib/common/api.js
var require_api2 = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/common/api.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __exportStar = exports2 && exports2.__exportStar || function(m, exports3) {
      for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports3, p)) __createBinding(exports3, m, p);
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.LSPErrorCodes = exports2.createProtocolConnection = void 0;
    __exportStar(require_api(), exports2);
    __exportStar((init_main(), __toCommonJS(main_exports)), exports2);
    __exportStar(require_messages2(), exports2);
    __exportStar(require_protocol(), exports2);
    var connection_1 = require_connection2();
    Object.defineProperty(exports2, "createProtocolConnection", { enumerable: true, get: function() {
      return connection_1.createProtocolConnection;
    } });
    var LSPErrorCodes;
    (function(LSPErrorCodes2) {
      LSPErrorCodes2.lspReservedErrorRangeStart = -32899;
      LSPErrorCodes2.RequestFailed = -32803;
      LSPErrorCodes2.ServerCancelled = -32802;
      LSPErrorCodes2.ContentModified = -32801;
      LSPErrorCodes2.RequestCancelled = -32800;
      LSPErrorCodes2.lspReservedErrorRangeEnd = -32800;
    })(LSPErrorCodes || (exports2.LSPErrorCodes = LSPErrorCodes = {}));
  }
});

// node_modules/vscode-languageclient/lib/common/utils/async.js
var require_async = __commonJS({
  "node_modules/vscode-languageclient/lib/common/utils/async.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.Semaphore = exports2.Delayer = void 0;
    exports2.setTestMode = setTestMode;
    exports2.clearTestMode = clearTestMode;
    exports2.map = map;
    exports2.mapAsync = mapAsync;
    exports2.forEach = forEach;
    var vscode_languageserver_protocol_1 = require_api2();
    var Delayer = class {
      defaultDelay;
      timeout;
      completionPromise;
      onSuccess;
      task;
      constructor(defaultDelay) {
        this.defaultDelay = defaultDelay;
        this.timeout = void 0;
        this.completionPromise = void 0;
        this.onSuccess = void 0;
        this.task = void 0;
      }
      trigger(task, delay = this.defaultDelay) {
        this.task = task;
        if (delay >= 0) {
          this.cancelTimeout();
        }
        if (!this.completionPromise) {
          this.completionPromise = new Promise((resolve) => {
            this.onSuccess = resolve;
          }).then(() => {
            this.completionPromise = void 0;
            this.onSuccess = void 0;
            const result = this.task();
            this.task = void 0;
            return result;
          });
        }
        if (delay >= 0 || this.timeout === void 0) {
          this.timeout = (0, vscode_languageserver_protocol_1.RAL)().timer.setTimeout(() => {
            this.timeout = void 0;
            this.onSuccess(void 0);
          }, delay >= 0 ? delay : this.defaultDelay);
        }
        return this.completionPromise;
      }
      forceDelivery() {
        if (!this.completionPromise) {
          return void 0;
        }
        this.cancelTimeout();
        const result = this.task();
        this.completionPromise = void 0;
        this.onSuccess = void 0;
        this.task = void 0;
        return result;
      }
      isTriggered() {
        return this.timeout !== void 0;
      }
      cancel() {
        this.cancelTimeout();
        this.completionPromise = void 0;
      }
      cancelTimeout() {
        if (this.timeout !== void 0) {
          this.timeout.dispose();
          this.timeout = void 0;
        }
      }
    };
    exports2.Delayer = Delayer;
    var Semaphore = class {
      _capacity;
      _active;
      _waiting;
      constructor(capacity = 1) {
        if (capacity <= 0) {
          throw new Error("Capacity must be greater than 0");
        }
        this._capacity = capacity;
        this._active = 0;
        this._waiting = [];
      }
      lock(thunk) {
        return new Promise((resolve, reject) => {
          this._waiting.push({ thunk, resolve, reject });
          this.runNext();
        });
      }
      get active() {
        return this._active;
      }
      runNext() {
        if (this._waiting.length === 0 || this._active === this._capacity) {
          return;
        }
        (0, vscode_languageserver_protocol_1.RAL)().timer.setImmediate(() => this.doRunNext());
      }
      doRunNext() {
        if (this._waiting.length === 0 || this._active === this._capacity) {
          return;
        }
        const next = this._waiting.shift();
        this._active++;
        if (this._active > this._capacity) {
          throw new Error(`To many thunks active`);
        }
        try {
          const result = next.thunk();
          if (result instanceof Promise) {
            result.then((value) => {
              this._active--;
              next.resolve(value);
              this.runNext();
            }, (err) => {
              this._active--;
              next.reject(err);
              this.runNext();
            });
          } else {
            this._active--;
            next.resolve(result);
            this.runNext();
          }
        } catch (err) {
          this._active--;
          next.reject(err);
          this.runNext();
        }
      }
    };
    exports2.Semaphore = Semaphore;
    var $test = false;
    function setTestMode() {
      $test = true;
    }
    function clearTestMode() {
      $test = false;
    }
    var defaultYieldTimeout = 15;
    var Timer = class {
      yieldAfter;
      startTime;
      counter;
      total;
      counterInterval;
      constructor(yieldAfter = defaultYieldTimeout) {
        this.yieldAfter = $test === true ? Math.max(yieldAfter, 2) : Math.max(yieldAfter, defaultYieldTimeout);
        this.startTime = Date.now();
        this.counter = 0;
        this.total = 0;
        this.counterInterval = 1;
      }
      start() {
        this.counter = 0;
        this.total = 0;
        this.counterInterval = 1;
        this.startTime = Date.now();
      }
      shouldYield() {
        if (++this.counter >= this.counterInterval) {
          const timeTaken = Date.now() - this.startTime;
          const timeLeft = Math.max(0, this.yieldAfter - timeTaken);
          this.total += this.counter;
          this.counter = 0;
          if (timeTaken >= this.yieldAfter || timeLeft <= 1) {
            this.counterInterval = 1;
            this.total = 0;
            return true;
          } else {
            switch (timeTaken) {
              case 0:
              case 1:
                this.counterInterval = this.total * 2;
                break;
            }
          }
        }
        return false;
      }
    };
    async function map(items, func, token, options) {
      if (items.length === 0) {
        return [];
      }
      const result = new Array(items.length);
      const timer = new Timer(options?.yieldAfter);
      function convertBatch(start) {
        timer.start();
        for (let i = start; i < items.length; i++) {
          result[i] = func(items[i]);
          if (timer.shouldYield()) {
            options?.yieldCallback && options.yieldCallback();
            return i + 1;
          }
        }
        return -1;
      }
      let index = convertBatch(0);
      while (index !== -1) {
        if (token !== void 0 && token.isCancellationRequested) {
          break;
        }
        index = await new Promise((resolve) => {
          (0, vscode_languageserver_protocol_1.RAL)().timer.setImmediate(() => {
            if (token !== void 0 && token.isCancellationRequested) {
              resolve(-1);
            } else {
              resolve(convertBatch(index));
            }
          });
        });
      }
      return result;
    }
    async function mapAsync(items, func, token, options) {
      if (items.length === 0) {
        return [];
      }
      const result = new Array(items.length);
      const timer = new Timer(options?.yieldAfter);
      async function convertBatch(start) {
        timer.start();
        for (let i = start; i < items.length; i++) {
          result[i] = await func(items[i], token);
          if (timer.shouldYield()) {
            options?.yieldCallback && options.yieldCallback();
            return i + 1;
          }
        }
        return -1;
      }
      let index = await convertBatch(0);
      while (index !== -1) {
        if (token !== void 0 && token.isCancellationRequested) {
          break;
        }
        index = await new Promise((resolve) => {
          (0, vscode_languageserver_protocol_1.RAL)().timer.setImmediate(() => {
            if (token !== void 0 && token.isCancellationRequested) {
              resolve(-1);
            } else {
              resolve(convertBatch(index));
            }
          });
        });
      }
      return result;
    }
    async function forEach(items, func, token, options) {
      if (items.length === 0) {
        return;
      }
      const timer = new Timer(options?.yieldAfter);
      function runBatch(start) {
        timer.start();
        for (let i = start; i < items.length; i++) {
          func(items[i]);
          if (timer.shouldYield()) {
            options?.yieldCallback && options.yieldCallback();
            return i + 1;
          }
        }
        return -1;
      }
      let index = runBatch(0);
      while (index !== -1) {
        if (token !== void 0 && token.isCancellationRequested) {
          break;
        }
        index = await new Promise((resolve) => {
          (0, vscode_languageserver_protocol_1.RAL)().timer.setImmediate(() => {
            if (token !== void 0 && token.isCancellationRequested) {
              resolve(-1);
            } else {
              resolve(runBatch(index));
            }
          });
        });
      }
    }
  }
});

// node_modules/vscode-languageclient/lib/common/protocolCompletionItem.js
var require_protocolCompletionItem = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolCompletionItem.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolCompletionItem = class extends code.CompletionItem {
      data;
      fromEdit;
      documentationFormat;
      originalItemKind;
      deprecated;
      insertTextMode;
      constructor(label) {
        super(label);
      }
    };
    exports2.default = ProtocolCompletionItem;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolCodeLens.js
var require_protocolCodeLens = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolCodeLens.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolCodeLens = class extends code.CodeLens {
      data;
      constructor(range) {
        super(range);
      }
    };
    exports2.default = ProtocolCodeLens;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolDocumentLink.js
var require_protocolDocumentLink = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolDocumentLink.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolDocumentLink = class extends code.DocumentLink {
      data;
      constructor(range, target) {
        super(range, target);
      }
    };
    exports2.default = ProtocolDocumentLink;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolCodeAction.js
var require_protocolCodeAction = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolCodeAction.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var vscode2 = __importStar(require("vscode"));
    var ProtocolCodeAction = class extends vscode2.CodeAction {
      data;
      constructor(title, data) {
        super(title);
        this.data = data;
      }
    };
    exports2.default = ProtocolCodeAction;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolDiagnostic.js
var require_protocolDiagnostic = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolDiagnostic.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ProtocolDiagnostic = exports2.DiagnosticCode = void 0;
    var vscode2 = __importStar(require("vscode"));
    var Is2 = __importStar(require_is());
    var DiagnosticCode;
    (function(DiagnosticCode2) {
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && (Is2.number(candidate.value) || Is2.string(candidate.value)) && Is2.string(candidate.target);
      }
      DiagnosticCode2.is = is;
    })(DiagnosticCode || (exports2.DiagnosticCode = DiagnosticCode = {}));
    var ProtocolDiagnostic = class extends vscode2.Diagnostic {
      data;
      hasDiagnosticCode;
      constructor(range, message, severity, data) {
        super(range, message, severity);
        this.data = data;
        this.hasDiagnosticCode = false;
      }
    };
    exports2.ProtocolDiagnostic = ProtocolDiagnostic;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolCallHierarchyItem.js
var require_protocolCallHierarchyItem = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolCallHierarchyItem.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolCallHierarchyItem = class extends code.CallHierarchyItem {
      data;
      constructor(kind, name, detail, uri, range, selectionRange, data) {
        super(kind, name, detail, uri, range, selectionRange);
        if (data !== void 0) {
          this.data = data;
        }
      }
    };
    exports2.default = ProtocolCallHierarchyItem;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolTypeHierarchyItem.js
var require_protocolTypeHierarchyItem = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolTypeHierarchyItem.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolTypeHierarchyItem = class extends code.TypeHierarchyItem {
      data;
      constructor(kind, name, detail, uri, range, selectionRange, data) {
        super(kind, name, detail, uri, range, selectionRange);
        if (data !== void 0) {
          this.data = data;
        }
      }
    };
    exports2.default = ProtocolTypeHierarchyItem;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolWorkspaceSymbol.js
var require_protocolWorkspaceSymbol = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolWorkspaceSymbol.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var WorkspaceSymbol2 = class extends code.SymbolInformation {
      data;
      hasRange;
      constructor(name, kind, containerName, locationOrUri, data) {
        const hasRange = !(locationOrUri instanceof code.Uri);
        super(name, kind, containerName, hasRange ? locationOrUri : new code.Location(locationOrUri, new code.Range(0, 0, 0, 0)));
        this.hasRange = hasRange;
        if (data !== void 0) {
          this.data = data;
        }
      }
    };
    exports2.default = WorkspaceSymbol2;
  }
});

// node_modules/vscode-languageclient/lib/common/protocolInlayHint.js
var require_protocolInlayHint = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolInlayHint.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    var code = __importStar(require("vscode"));
    var ProtocolInlayHint = class extends code.InlayHint {
      data;
      constructor(position, label, kind) {
        super(position, label, kind);
      }
    };
    exports2.default = ProtocolInlayHint;
  }
});

// node_modules/vscode-languageclient/lib/common/codeConverter.js
var require_codeConverter = __commonJS({
  "node_modules/vscode-languageclient/lib/common/codeConverter.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.createConverter = createConverter;
    var code = __importStar(require("vscode"));
    var proto = __importStar(require_api2());
    var Is2 = __importStar(require_is());
    var async = __importStar(require_async());
    var protocolCompletionItem_1 = __importDefault(require_protocolCompletionItem());
    var protocolCodeLens_1 = __importDefault(require_protocolCodeLens());
    var protocolDocumentLink_1 = __importDefault(require_protocolDocumentLink());
    var protocolCodeAction_1 = __importDefault(require_protocolCodeAction());
    var protocolDiagnostic_1 = require_protocolDiagnostic();
    var protocolCallHierarchyItem_1 = __importDefault(require_protocolCallHierarchyItem());
    var protocolTypeHierarchyItem_1 = __importDefault(require_protocolTypeHierarchyItem());
    var protocolWorkspaceSymbol_1 = __importDefault(require_protocolWorkspaceSymbol());
    var protocolInlayHint_1 = __importDefault(require_protocolInlayHint());
    var InsertReplaceRange;
    (function(InsertReplaceRange2) {
      function is(value) {
        const candidate = value;
        return candidate && !!candidate.inserting && !!candidate.replacing;
      }
      InsertReplaceRange2.is = is;
    })(InsertReplaceRange || (InsertReplaceRange = {}));
    function createConverter(uriConverter) {
      const nullConverter = (value) => value.toString();
      const _uriConverter = uriConverter || nullConverter;
      function asUri(value) {
        return _uriConverter(value);
      }
      function asTextDocumentIdentifier(textDocument) {
        return {
          uri: _uriConverter(textDocument.uri)
        };
      }
      function asTextDocumentItem(textDocument) {
        return {
          uri: _uriConverter(textDocument.uri),
          languageId: textDocument.languageId,
          version: textDocument.version,
          text: textDocument.getText()
        };
      }
      function asVersionedTextDocumentIdentifier(textDocument) {
        return {
          uri: _uriConverter(textDocument.uri),
          version: textDocument.version
        };
      }
      function asOpenTextDocumentParams(textDocument) {
        return {
          textDocument: asTextDocumentItem(textDocument)
        };
      }
      function isTextDocumentChangeEvent(value) {
        const candidate = value;
        return !!candidate.document && !!candidate.contentChanges;
      }
      function isTextDocument(value) {
        const candidate = value;
        return !!candidate.uri && !!candidate.version;
      }
      function asChangeTextDocumentParams(arg0, arg1, arg2) {
        if (isTextDocument(arg0)) {
          const result = {
            textDocument: {
              uri: _uriConverter(arg0.uri),
              version: arg0.version
            },
            contentChanges: [{ text: arg0.getText() }]
          };
          return result;
        } else if (isTextDocumentChangeEvent(arg0)) {
          const uri = arg1;
          const version = arg2;
          const result = {
            textDocument: {
              uri: _uriConverter(uri),
              version
            },
            contentChanges: arg0.contentChanges.map((change) => {
              const range = change.range;
              return {
                range: {
                  start: { line: range.start.line, character: range.start.character },
                  end: { line: range.end.line, character: range.end.character }
                },
                rangeLength: change.rangeLength,
                text: change.text
              };
            })
          };
          return result;
        } else {
          throw Error("Unsupported text document change parameter");
        }
      }
      function asCloseTextDocumentParams(textDocument) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument)
        };
      }
      function asSaveTextDocumentParams(textDocument, includeContent = false) {
        const result = {
          textDocument: asTextDocumentIdentifier(textDocument)
        };
        if (includeContent) {
          result.text = textDocument.getText();
        }
        return result;
      }
      function asTextDocumentSaveReason(reason) {
        switch (reason) {
          case code.TextDocumentSaveReason.Manual:
            return proto.TextDocumentSaveReason.Manual;
          case code.TextDocumentSaveReason.AfterDelay:
            return proto.TextDocumentSaveReason.AfterDelay;
          case code.TextDocumentSaveReason.FocusOut:
            return proto.TextDocumentSaveReason.FocusOut;
        }
        return proto.TextDocumentSaveReason.Manual;
      }
      function asWillSaveTextDocumentParams(event) {
        return {
          textDocument: asTextDocumentIdentifier(event.document),
          reason: asTextDocumentSaveReason(event.reason)
        };
      }
      function asDidCreateFilesParams(event) {
        return {
          files: event.files.map((fileUri) => ({
            uri: _uriConverter(fileUri)
          }))
        };
      }
      function asDidRenameFilesParams(event) {
        return {
          files: event.files.map((file) => ({
            oldUri: _uriConverter(file.oldUri),
            newUri: _uriConverter(file.newUri)
          }))
        };
      }
      function asDidDeleteFilesParams(event) {
        return {
          files: event.files.map((fileUri) => ({
            uri: _uriConverter(fileUri)
          }))
        };
      }
      function asWillCreateFilesParams(event) {
        return {
          files: event.files.map((fileUri) => ({
            uri: _uriConverter(fileUri)
          }))
        };
      }
      function asWillRenameFilesParams(event) {
        return {
          files: event.files.map((file) => ({
            oldUri: _uriConverter(file.oldUri),
            newUri: _uriConverter(file.newUri)
          }))
        };
      }
      function asWillDeleteFilesParams(event) {
        return {
          files: event.files.map((fileUri) => ({
            uri: _uriConverter(fileUri)
          }))
        };
      }
      function asTextDocumentPositionParams(textDocument, position) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument),
          position: asWorkerPosition(position)
        };
      }
      function asCompletionTriggerKind(triggerKind) {
        switch (triggerKind) {
          case code.CompletionTriggerKind.TriggerCharacter:
            return proto.CompletionTriggerKind.TriggerCharacter;
          case code.CompletionTriggerKind.TriggerForIncompleteCompletions:
            return proto.CompletionTriggerKind.TriggerForIncompleteCompletions;
          default:
            return proto.CompletionTriggerKind.Invoked;
        }
      }
      function asCompletionParams(textDocument, position, context) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument),
          position: asWorkerPosition(position),
          context: {
            triggerKind: asCompletionTriggerKind(context.triggerKind),
            triggerCharacter: context.triggerCharacter
          }
        };
      }
      function asSignatureHelpTriggerKind(triggerKind) {
        switch (triggerKind) {
          case code.SignatureHelpTriggerKind.Invoke:
            return proto.SignatureHelpTriggerKind.Invoked;
          case code.SignatureHelpTriggerKind.TriggerCharacter:
            return proto.SignatureHelpTriggerKind.TriggerCharacter;
          case code.SignatureHelpTriggerKind.ContentChange:
            return proto.SignatureHelpTriggerKind.ContentChange;
        }
      }
      function asParameterInformation(value) {
        return {
          label: value.label
        };
      }
      function asParameterInformations(values) {
        return values.map(asParameterInformation);
      }
      function asSignatureInformation(value) {
        return {
          label: value.label,
          parameters: asParameterInformations(value.parameters)
        };
      }
      function asSignatureInformations(values) {
        return values.map(asSignatureInformation);
      }
      function asSignatureHelp(value) {
        if (value === void 0) {
          return value;
        }
        return {
          signatures: asSignatureInformations(value.signatures),
          activeSignature: value.activeSignature,
          activeParameter: value.activeParameter
        };
      }
      function asSignatureHelpParams(textDocument, position, context) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument),
          position: asWorkerPosition(position),
          context: {
            isRetrigger: context.isRetrigger,
            triggerCharacter: context.triggerCharacter,
            triggerKind: asSignatureHelpTriggerKind(context.triggerKind),
            activeSignatureHelp: asSignatureHelp(context.activeSignatureHelp)
          }
        };
      }
      function asWorkerPosition(position) {
        return { line: position.line, character: position.character };
      }
      function asPosition(value) {
        if (value === void 0 || value === null) {
          return value;
        }
        return { line: value.line > proto.uinteger.MAX_VALUE ? proto.uinteger.MAX_VALUE : value.line, character: value.character > proto.uinteger.MAX_VALUE ? proto.uinteger.MAX_VALUE : value.character };
      }
      function asPositions(values, token) {
        return async.map(values, asPosition, token);
      }
      function asPositionsSync(values) {
        return values.map(asPosition);
      }
      function asRange(value) {
        if (value === void 0 || value === null) {
          return value;
        }
        return { start: asPosition(value.start), end: asPosition(value.end) };
      }
      function asRanges(values) {
        return values.map(asRange);
      }
      function asLocation(value) {
        if (value === void 0 || value === null) {
          return value;
        }
        return proto.Location.create(asUri(value.uri), asRange(value.range));
      }
      function asDiagnosticSeverity(value) {
        switch (value) {
          case code.DiagnosticSeverity.Error:
            return proto.DiagnosticSeverity.Error;
          case code.DiagnosticSeverity.Warning:
            return proto.DiagnosticSeverity.Warning;
          case code.DiagnosticSeverity.Information:
            return proto.DiagnosticSeverity.Information;
          case code.DiagnosticSeverity.Hint:
            return proto.DiagnosticSeverity.Hint;
        }
      }
      function asDiagnosticTags(tags) {
        if (!tags) {
          return void 0;
        }
        const result = [];
        for (const tag of tags) {
          const converted = asDiagnosticTag(tag);
          if (converted !== void 0) {
            result.push(converted);
          }
        }
        return result.length > 0 ? result : void 0;
      }
      function asDiagnosticTag(tag) {
        switch (tag) {
          case code.DiagnosticTag.Unnecessary:
            return proto.DiagnosticTag.Unnecessary;
          case code.DiagnosticTag.Deprecated:
            return proto.DiagnosticTag.Deprecated;
          default:
            return void 0;
        }
      }
      function asRelatedInformation(item) {
        return {
          message: item.message,
          location: asLocation(item.location)
        };
      }
      function asRelatedInformations(items) {
        return items.map(asRelatedInformation);
      }
      function asDiagnosticCode(value) {
        if (value === void 0 || value === null) {
          return void 0;
        }
        if (Is2.number(value) || Is2.string(value)) {
          return value;
        }
        return { value: value.value, target: asUri(value.target) };
      }
      function asDiagnostic(item) {
        const result = proto.Diagnostic.create(asRange(item.range), item.message);
        const protocolDiagnostic = item instanceof protocolDiagnostic_1.ProtocolDiagnostic ? item : void 0;
        if (protocolDiagnostic !== void 0 && protocolDiagnostic.data !== void 0) {
          result.data = protocolDiagnostic.data;
        }
        const code2 = asDiagnosticCode(item.code);
        if (protocolDiagnostic_1.DiagnosticCode.is(code2)) {
          if (protocolDiagnostic !== void 0 && protocolDiagnostic.hasDiagnosticCode) {
            result.code = code2;
          } else {
            result.code = code2.value;
            result.codeDescription = { href: code2.target };
          }
        } else {
          result.code = code2;
        }
        if (Is2.number(item.severity)) {
          result.severity = asDiagnosticSeverity(item.severity);
        }
        if (Array.isArray(item.tags)) {
          result.tags = asDiagnosticTags(item.tags);
        }
        if (item.relatedInformation) {
          result.relatedInformation = asRelatedInformations(item.relatedInformation);
        }
        if (item.source) {
          result.source = item.source;
        }
        return result;
      }
      function asDiagnostics(items, token) {
        if (items === void 0 || items === null) {
          return items;
        }
        return async.map(items, asDiagnostic, token);
      }
      function asDiagnosticsSync(items) {
        if (items === void 0 || items === null) {
          return items;
        }
        return items.map(asDiagnostic);
      }
      function asDocumentation(format, documentation) {
        switch (format) {
          case "$string":
            return documentation;
          case proto.MarkupKind.PlainText:
            return { kind: format, value: documentation };
          case proto.MarkupKind.Markdown:
            return { kind: format, value: documentation.value };
          default:
            return `Unsupported Markup content received. Kind is: ${format}`;
        }
      }
      function asCompletionItemTag(tag) {
        switch (tag) {
          case code.CompletionItemTag.Deprecated:
            return proto.CompletionItemTag.Deprecated;
        }
        return void 0;
      }
      function asCompletionItemTags(tags) {
        if (tags === void 0) {
          return tags;
        }
        const result = [];
        for (const tag of tags) {
          const converted = asCompletionItemTag(tag);
          if (converted !== void 0) {
            result.push(converted);
          }
        }
        return result;
      }
      function asCompletionItemKind(value, original) {
        if (original !== void 0) {
          return original;
        }
        return value + 1;
      }
      function asCompletionItem(item, labelDetailsSupport = false) {
        let label;
        let labelDetails;
        if (Is2.string(item.label)) {
          label = item.label;
        } else {
          label = item.label.label;
          if (labelDetailsSupport && (item.label.detail !== void 0 || item.label.description !== void 0)) {
            labelDetails = { detail: item.label.detail, description: item.label.description };
          }
        }
        const result = { label };
        if (labelDetails !== void 0) {
          result.labelDetails = labelDetails;
        }
        const protocolItem = item instanceof protocolCompletionItem_1.default ? item : void 0;
        if (item.detail) {
          result.detail = item.detail;
        }
        if (item.documentation) {
          if (!protocolItem || protocolItem.documentationFormat === "$string") {
            result.documentation = item.documentation;
          } else {
            result.documentation = asDocumentation(protocolItem.documentationFormat, item.documentation);
          }
        }
        if (item.filterText) {
          result.filterText = item.filterText;
        }
        fillPrimaryInsertText(result, item);
        if (Is2.number(item.kind)) {
          result.kind = asCompletionItemKind(item.kind, protocolItem && protocolItem.originalItemKind);
        }
        if (item.sortText) {
          result.sortText = item.sortText;
        }
        if (item.additionalTextEdits) {
          result.additionalTextEdits = asTextEdits(item.additionalTextEdits);
        }
        if (item.commitCharacters) {
          result.commitCharacters = item.commitCharacters.slice();
        }
        if (item.command) {
          result.command = asCommand(item.command);
        }
        if (item.preselect === true || item.preselect === false) {
          result.preselect = item.preselect;
        }
        const tags = asCompletionItemTags(item.tags);
        if (protocolItem) {
          if (protocolItem.data !== void 0) {
            result.data = protocolItem.data;
          }
          if (protocolItem.deprecated === true || protocolItem.deprecated === false) {
            if (protocolItem.deprecated === true && tags !== void 0 && tags.length > 0) {
              const index = tags.indexOf(code.CompletionItemTag.Deprecated);
              if (index !== -1) {
                tags.splice(index, 1);
              }
            }
            result.deprecated = protocolItem.deprecated;
          }
          if (protocolItem.insertTextMode !== void 0) {
            result.insertTextMode = protocolItem.insertTextMode;
          }
        }
        if (tags !== void 0 && tags.length > 0) {
          result.tags = tags;
        }
        if (result.insertTextMode === void 0 && item.keepWhitespace === true) {
          result.insertTextMode = proto.InsertTextMode.adjustIndentation;
        }
        return result;
      }
      function fillPrimaryInsertText(target, source) {
        let format = proto.InsertTextFormat.PlainText;
        let text = void 0;
        let range = void 0;
        if (source.textEdit) {
          text = source.textEdit.newText;
          range = source.textEdit.range;
        } else if (source.insertText instanceof code.SnippetString) {
          format = proto.InsertTextFormat.Snippet;
          text = source.insertText.value;
        } else {
          text = source.insertText;
        }
        if (source.range) {
          range = source.range;
        }
        target.insertTextFormat = format;
        if (source.fromEdit && text !== void 0 && range !== void 0) {
          target.textEdit = asCompletionTextEdit(text, range);
        } else {
          target.insertText = text;
        }
      }
      function asCompletionTextEdit(newText, range) {
        if (InsertReplaceRange.is(range)) {
          return proto.InsertReplaceEdit.create(newText, asRange(range.inserting), asRange(range.replacing));
        } else {
          return { newText, range: asRange(range) };
        }
      }
      function asTextEdit(edit) {
        return { range: asRange(edit.range), newText: edit.newText };
      }
      function asTextEdits(edits) {
        if (edits === void 0 || edits === null) {
          return edits;
        }
        return edits.map(asTextEdit);
      }
      function asSymbolKind(item) {
        if (item <= code.SymbolKind.TypeParameter) {
          return item + 1;
        }
        return proto.SymbolKind.Property;
      }
      function asSymbolTag(item) {
        return item;
      }
      function asSymbolTags(items) {
        return items.map(asSymbolTag);
      }
      function asReferenceParams(textDocument, position, options) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument),
          position: asWorkerPosition(position),
          context: { includeDeclaration: options.includeDeclaration }
        };
      }
      async function asCodeAction(item, token) {
        const result = proto.CodeAction.create(item.title);
        if (item instanceof protocolCodeAction_1.default && item.data !== void 0) {
          result.data = item.data;
        }
        if (item.kind !== void 0) {
          result.kind = asCodeActionKind(item.kind);
        }
        if (item.diagnostics !== void 0) {
          result.diagnostics = await asDiagnostics(item.diagnostics, token);
        }
        if (item.edit !== void 0) {
          throw new Error(`VS Code code actions can only be converted to a protocol code action without an edit.`);
        }
        if (item.command !== void 0) {
          result.command = asCommand(item.command);
        }
        if (item.isPreferred !== void 0) {
          result.isPreferred = item.isPreferred;
        }
        if (item.disabled !== void 0) {
          result.disabled = { reason: item.disabled.reason };
        }
        if (item.isAI) {
          result.tags ??= [];
          result.tags.push(proto.CodeActionTag.LLMGenerated);
        }
        return result;
      }
      function asCodeActionSync(item) {
        const result = proto.CodeAction.create(item.title);
        if (item instanceof protocolCodeAction_1.default && item.data !== void 0) {
          result.data = item.data;
        }
        if (item.kind !== void 0) {
          result.kind = asCodeActionKind(item.kind);
        }
        if (item.diagnostics !== void 0) {
          result.diagnostics = asDiagnosticsSync(item.diagnostics);
        }
        if (item.edit !== void 0) {
          throw new Error(`VS Code code actions can only be converted to a protocol code action without an edit.`);
        }
        if (item.command !== void 0) {
          result.command = asCommand(item.command);
        }
        if (item.isPreferred !== void 0) {
          result.isPreferred = item.isPreferred;
        }
        if (item.disabled !== void 0) {
          result.disabled = { reason: item.disabled.reason };
        }
        if (item.isAI) {
          result.tags ??= [];
          result.tags.push(proto.CodeActionTag.LLMGenerated);
        }
        return result;
      }
      async function asCodeActionContext(context, token) {
        if (context === void 0 || context === null) {
          return context;
        }
        let only;
        if (context.only && Is2.string(context.only.value)) {
          only = [context.only.value];
        }
        return proto.CodeActionContext.create(await asDiagnostics(context.diagnostics, token), only, asCodeActionTriggerKind(context.triggerKind));
      }
      function asCodeActionContextSync(context) {
        if (context === void 0 || context === null) {
          return context;
        }
        let only;
        if (context.only && Is2.string(context.only.value)) {
          only = [context.only.value];
        }
        return proto.CodeActionContext.create(asDiagnosticsSync(context.diagnostics), only, asCodeActionTriggerKind(context.triggerKind));
      }
      function asCodeActionTriggerKind(kind) {
        switch (kind) {
          case code.CodeActionTriggerKind.Invoke:
            return proto.CodeActionTriggerKind.Invoked;
          case code.CodeActionTriggerKind.Automatic:
            return proto.CodeActionTriggerKind.Automatic;
          default:
            return void 0;
        }
      }
      function asCodeActionKind(item) {
        if (item === void 0 || item === null) {
          return void 0;
        }
        return item.value;
      }
      function asInlineValueContext(context) {
        return proto.InlineValueContext.create(context.frameId, asRange(context.stoppedLocation));
      }
      function asInlineCompletionParams(document, position, context) {
        return {
          textDocument: asTextDocumentIdentifier(document),
          position: asPosition(position),
          context: asInlineCompletionContext(context)
        };
      }
      function asInlineCompletionContext(context) {
        return {
          triggerKind: asInlineCompletionTriggerKind(context.triggerKind),
          selectedCompletionInfo: asSelectedCompletionInfo(context.selectedCompletionInfo)
        };
      }
      function asInlineCompletionTriggerKind(kind) {
        switch (kind) {
          case code.InlineCompletionTriggerKind.Invoke:
            return proto.InlineCompletionTriggerKind.Invoked;
          case code.InlineCompletionTriggerKind.Automatic:
            return proto.InlineCompletionTriggerKind.Automatic;
        }
      }
      function asSelectedCompletionInfo(info) {
        if (info === void 0 || info === null) {
          return void 0;
        }
        return { range: asRange(info.range), text: info.text };
      }
      function asCommand(item) {
        const result = proto.Command.create(item.title, item.command);
        if (item.tooltip) {
          result.tooltip = item.tooltip;
        }
        if (item.arguments) {
          result.arguments = item.arguments;
        }
        return result;
      }
      function asCodeLens(item) {
        const result = proto.CodeLens.create(asRange(item.range));
        if (item.command) {
          result.command = asCommand(item.command);
        }
        if (item instanceof protocolCodeLens_1.default) {
          if (item.data) {
            result.data = item.data;
          }
        }
        return result;
      }
      function asFormattingOptions(options, fileOptions) {
        const result = { tabSize: options.tabSize, insertSpaces: options.insertSpaces };
        if (fileOptions.trimTrailingWhitespace) {
          result.trimTrailingWhitespace = true;
        }
        if (fileOptions.trimFinalNewlines) {
          result.trimFinalNewlines = true;
        }
        if (fileOptions.insertFinalNewline) {
          result.insertFinalNewline = true;
        }
        return result;
      }
      function asDocumentSymbolParams(textDocument) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument)
        };
      }
      function asCodeLensParams(textDocument) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument)
        };
      }
      function asDocumentLink(item) {
        const result = proto.DocumentLink.create(asRange(item.range));
        if (item.target) {
          result.target = asUri(item.target);
        }
        if (item.tooltip !== void 0) {
          result.tooltip = item.tooltip;
        }
        const protocolItem = item instanceof protocolDocumentLink_1.default ? item : void 0;
        if (protocolItem && protocolItem.data) {
          result.data = protocolItem.data;
        }
        return result;
      }
      function asDocumentLinkParams(textDocument) {
        return {
          textDocument: asTextDocumentIdentifier(textDocument)
        };
      }
      function asCallHierarchyItem(value) {
        const result = {
          name: value.name,
          kind: asSymbolKind(value.kind),
          uri: asUri(value.uri),
          range: asRange(value.range),
          selectionRange: asRange(value.selectionRange)
        };
        if (value.detail !== void 0 && value.detail.length > 0) {
          result.detail = value.detail;
        }
        if (value.tags !== void 0) {
          result.tags = asSymbolTags(value.tags);
        }
        if (value instanceof protocolCallHierarchyItem_1.default && value.data !== void 0) {
          result.data = value.data;
        }
        return result;
      }
      function asTypeHierarchyItem(value) {
        const result = {
          name: value.name,
          kind: asSymbolKind(value.kind),
          uri: asUri(value.uri),
          range: asRange(value.range),
          selectionRange: asRange(value.selectionRange)
        };
        if (value.detail !== void 0 && value.detail.length > 0) {
          result.detail = value.detail;
        }
        if (value.tags !== void 0) {
          result.tags = asSymbolTags(value.tags);
        }
        if (value instanceof protocolTypeHierarchyItem_1.default && value.data !== void 0) {
          result.data = value.data;
        }
        return result;
      }
      function asWorkspaceSymbol(item) {
        const result = item instanceof protocolWorkspaceSymbol_1.default ? { name: item.name, kind: asSymbolKind(item.kind), location: item.hasRange ? asLocation(item.location) : { uri: _uriConverter(item.location.uri) }, data: item.data } : { name: item.name, kind: asSymbolKind(item.kind), location: asLocation(item.location) };
        if (item.tags !== void 0) {
          result.tags = asSymbolTags(item.tags);
        }
        if (item.containerName !== "") {
          result.containerName = item.containerName;
        }
        return result;
      }
      function asInlayHint(item) {
        const label = typeof item.label === "string" ? item.label : item.label.map(asInlayHintLabelPart);
        const result = proto.InlayHint.create(asPosition(item.position), label);
        if (item.kind !== void 0) {
          result.kind = item.kind;
        }
        if (item.textEdits !== void 0) {
          result.textEdits = asTextEdits(item.textEdits);
        }
        if (item.tooltip !== void 0) {
          result.tooltip = asTooltip(item.tooltip);
        }
        if (item.paddingLeft !== void 0) {
          result.paddingLeft = item.paddingLeft;
        }
        if (item.paddingRight !== void 0) {
          result.paddingRight = item.paddingRight;
        }
        if (item instanceof protocolInlayHint_1.default && item.data !== void 0) {
          result.data = item.data;
        }
        return result;
      }
      function asInlayHintLabelPart(item) {
        const result = proto.InlayHintLabelPart.create(item.value);
        if (item.location !== void 0) {
          result.location = asLocation(item.location);
        }
        if (item.command !== void 0) {
          result.command = asCommand(item.command);
        }
        if (item.tooltip !== void 0) {
          result.tooltip = asTooltip(item.tooltip);
        }
        return result;
      }
      function asTooltip(value) {
        if (typeof value === "string") {
          return value;
        }
        const result = {
          kind: proto.MarkupKind.Markdown,
          value: value.value
        };
        return result;
      }
      return {
        asUri,
        asTextDocumentIdentifier,
        asTextDocumentItem,
        asVersionedTextDocumentIdentifier,
        asOpenTextDocumentParams,
        asChangeTextDocumentParams,
        asCloseTextDocumentParams,
        asSaveTextDocumentParams,
        asWillSaveTextDocumentParams,
        asDidCreateFilesParams,
        asDidRenameFilesParams,
        asDidDeleteFilesParams,
        asWillCreateFilesParams,
        asWillRenameFilesParams,
        asWillDeleteFilesParams,
        asTextDocumentPositionParams,
        asCompletionParams,
        asSignatureHelpParams,
        asWorkerPosition,
        asRange,
        asRanges,
        asPosition,
        asPositions,
        asPositionsSync,
        asLocation,
        asDiagnosticSeverity,
        asDiagnosticTag,
        asDiagnostic,
        asDiagnostics,
        asDiagnosticsSync,
        asCompletionItem,
        asTextEdit,
        asSymbolKind,
        asSymbolTag,
        asSymbolTags,
        asReferenceParams,
        asCodeAction,
        asCodeActionSync,
        asCodeActionContext,
        asCodeActionContextSync,
        asInlineValueContext,
        asCommand,
        asCodeLens,
        asFormattingOptions,
        asDocumentSymbolParams,
        asCodeLensParams,
        asDocumentLink,
        asDocumentLinkParams,
        asCallHierarchyItem,
        asTypeHierarchyItem,
        asInlayHint,
        asWorkspaceSymbol,
        asInlineCompletionParams,
        asInlineCompletionContext
      };
    }
  }
});

// node_modules/vscode-languageclient/lib/common/protocolConverter.js
var require_protocolConverter = __commonJS({
  "node_modules/vscode-languageclient/lib/common/protocolConverter.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.createConverter = createConverter;
    var code = __importStar(require("vscode"));
    var ls = __importStar(require_api2());
    var Is2 = __importStar(require_is());
    var async = __importStar(require_async());
    var protocolCompletionItem_1 = __importDefault(require_protocolCompletionItem());
    var protocolCodeLens_1 = __importDefault(require_protocolCodeLens());
    var protocolDocumentLink_1 = __importDefault(require_protocolDocumentLink());
    var protocolCodeAction_1 = __importDefault(require_protocolCodeAction());
    var protocolDiagnostic_1 = require_protocolDiagnostic();
    var protocolCallHierarchyItem_1 = __importDefault(require_protocolCallHierarchyItem());
    var protocolTypeHierarchyItem_1 = __importDefault(require_protocolTypeHierarchyItem());
    var protocolWorkspaceSymbol_1 = __importDefault(require_protocolWorkspaceSymbol());
    var protocolInlayHint_1 = __importDefault(require_protocolInlayHint());
    var vscode_languageserver_protocol_1 = require_api2();
    var CodeBlock;
    (function(CodeBlock2) {
      function is(value) {
        const candidate = value;
        return candidate && Is2.string(candidate.language) && Is2.string(candidate.value);
      }
      CodeBlock2.is = is;
    })(CodeBlock || (CodeBlock = {}));
    function createConverter(uriConverter, trustMarkdown, supportHtml, supportThemeIcons) {
      const nullConverter = (value) => code.Uri.parse(value);
      const _uriConverter = uriConverter || nullConverter;
      function asUri(value) {
        return _uriConverter(value);
      }
      function asDocumentSelector(selector) {
        const result = [];
        for (const filter of selector) {
          if (typeof filter === "string") {
            result.push(filter);
          } else if (vscode_languageserver_protocol_1.NotebookCellTextDocumentFilter.is(filter)) {
            if (typeof filter.notebook === "string") {
              result.push({ notebookType: filter.notebook, language: filter.language });
            } else {
              const notebookType = filter.notebook.notebookType ?? "*";
              result.push({ notebookType, scheme: filter.notebook.scheme, pattern: asGlobPattern(filter.notebook.pattern), language: filter.language });
            }
          } else if (vscode_languageserver_protocol_1.TextDocumentFilter.is(filter)) {
            result.push({ language: filter.language, scheme: filter.scheme, pattern: asGlobPattern(filter.pattern) });
          }
        }
        return result;
      }
      async function asDiagnostics(diagnostics, token) {
        return async.map(diagnostics, asDiagnostic, token);
      }
      function asDiagnosticsSync(diagnostics) {
        const result = new Array(diagnostics.length);
        for (let i = 0; i < diagnostics.length; i++) {
          result[i] = asDiagnostic(diagnostics[i]);
        }
        return result;
      }
      function asDiagnostic(diagnostic) {
        const message = typeof diagnostic.message === "string" ? diagnostic.message : diagnostic.message.kind === "plaintext" ? diagnostic.message.value : "Received a markup diagnostic message but the client does not support it.";
        const result = new protocolDiagnostic_1.ProtocolDiagnostic(asRange(diagnostic.range), message, asDiagnosticSeverity(diagnostic.severity), diagnostic.data);
        if (diagnostic.code !== void 0) {
          if (typeof diagnostic.code === "string" || typeof diagnostic.code === "number") {
            if (ls.CodeDescription.is(diagnostic.codeDescription)) {
              result.code = {
                value: diagnostic.code,
                target: asUri(diagnostic.codeDescription.href)
              };
            } else {
              result.code = diagnostic.code;
            }
          } else if (protocolDiagnostic_1.DiagnosticCode.is(diagnostic.code)) {
            result.hasDiagnosticCode = true;
            const diagnosticCode2 = diagnostic.code;
            result.code = {
              value: diagnosticCode2.value,
              target: asUri(diagnosticCode2.target)
            };
          }
        }
        if (diagnostic.source) {
          result.source = diagnostic.source;
        }
        if (diagnostic.relatedInformation) {
          result.relatedInformation = asRelatedInformation(diagnostic.relatedInformation);
        }
        if (Array.isArray(diagnostic.tags)) {
          result.tags = asDiagnosticTags(diagnostic.tags);
        }
        return result;
      }
      function asRelatedInformation(relatedInformation) {
        const result = new Array(relatedInformation.length);
        for (let i = 0; i < relatedInformation.length; i++) {
          const info = relatedInformation[i];
          result[i] = new code.DiagnosticRelatedInformation(asLocation(info.location), info.message);
        }
        return result;
      }
      function asDiagnosticTags(tags) {
        if (!tags) {
          return void 0;
        }
        const result = [];
        for (const tag of tags) {
          const converted = asDiagnosticTag(tag);
          if (converted !== void 0) {
            result.push(converted);
          }
        }
        return result.length > 0 ? result : void 0;
      }
      function asDiagnosticTag(tag) {
        switch (tag) {
          case ls.DiagnosticTag.Unnecessary:
            return code.DiagnosticTag.Unnecessary;
          case ls.DiagnosticTag.Deprecated:
            return code.DiagnosticTag.Deprecated;
          default:
            return void 0;
        }
      }
      function asPosition(value) {
        return value ? new code.Position(value.line, value.character) : void 0;
      }
      function asRange(value) {
        return value ? new code.Range(value.start.line, value.start.character, value.end.line, value.end.character) : void 0;
      }
      async function asRanges(items, token) {
        return async.map(items, (range) => {
          return new code.Range(range.start.line, range.start.character, range.end.line, range.end.character);
        }, token);
      }
      function asDiagnosticSeverity(value) {
        if (value === void 0 || value === null) {
          return code.DiagnosticSeverity.Error;
        }
        switch (value) {
          case ls.DiagnosticSeverity.Error:
            return code.DiagnosticSeverity.Error;
          case ls.DiagnosticSeverity.Warning:
            return code.DiagnosticSeverity.Warning;
          case ls.DiagnosticSeverity.Information:
            return code.DiagnosticSeverity.Information;
          case ls.DiagnosticSeverity.Hint:
            return code.DiagnosticSeverity.Hint;
        }
        return code.DiagnosticSeverity.Error;
      }
      function asHoverContent(value) {
        if (Is2.string(value)) {
          return asMarkdownString(value);
        } else if (CodeBlock.is(value)) {
          const result = asMarkdownString();
          return result.appendCodeblock(value.value, value.language);
        } else if (Array.isArray(value)) {
          const result = [];
          for (const element of value) {
            const item = asMarkdownString();
            if (CodeBlock.is(element)) {
              item.appendCodeblock(element.value, element.language);
            } else {
              item.appendMarkdown(element);
            }
            result.push(item);
          }
          return result;
        } else {
          return asMarkdownString(value);
        }
      }
      function asDocumentation(value) {
        if (Is2.string(value)) {
          return value;
        } else {
          switch (value.kind) {
            case ls.MarkupKind.Markdown:
              return asMarkdownString(value.value);
            case ls.MarkupKind.PlainText:
              return value.value;
            default:
              return `Unsupported Markup content received. Kind is: ${value.kind}`;
          }
        }
      }
      function asMarkdownString(value) {
        let result;
        if (value === void 0 || typeof value === "string") {
          result = new code.MarkdownString(value);
        } else {
          switch (value.kind) {
            case ls.MarkupKind.Markdown:
              result = new code.MarkdownString(value.value);
              break;
            case ls.MarkupKind.PlainText:
              result = new code.MarkdownString();
              result.appendText(value.value);
              break;
            default:
              result = new code.MarkdownString();
              result.appendText(`Unsupported Markup content received. Kind is: ${value.kind}`);
              break;
          }
        }
        result.isTrusted = trustMarkdown;
        result.supportHtml = supportHtml;
        result.supportThemeIcons = supportThemeIcons;
        return result;
      }
      function asHover(hover) {
        if (!hover) {
          return void 0;
        }
        return new code.Hover(asHoverContent(hover.contents), asRange(hover.range));
      }
      async function asCompletionResult(value, allCommitCharacters, token) {
        if (!value) {
          return void 0;
        }
        if (Array.isArray(value)) {
          return async.map(value, (item) => asCompletionItem(item, allCommitCharacters), token);
        }
        const list = value;
        const { defaultRange, commitCharacters } = getCompletionItemDefaults(list, allCommitCharacters);
        const converted = await async.map(list.items, (item) => {
          return asCompletionItem(item, commitCharacters, list.applyKind?.commitCharacters, defaultRange, list.itemDefaults?.insertTextMode, list.itemDefaults?.insertTextFormat, list.itemDefaults?.data, list.applyKind?.data);
        }, token);
        return new code.CompletionList(converted, list.isIncomplete);
      }
      function getCompletionItemDefaults(list, allCommitCharacters) {
        const rangeDefaults = list.itemDefaults?.editRange;
        const commitCharacters = list.itemDefaults?.commitCharacters ?? allCommitCharacters;
        return ls.Range.is(rangeDefaults) ? { defaultRange: asRange(rangeDefaults), commitCharacters } : rangeDefaults !== void 0 ? { defaultRange: { inserting: asRange(rangeDefaults.insert), replacing: asRange(rangeDefaults.replace) }, commitCharacters } : { defaultRange: void 0, commitCharacters };
      }
      function asCompletionItemKind(value) {
        if (ls.CompletionItemKind.Text <= value && value <= ls.CompletionItemKind.TypeParameter) {
          return [value - 1, void 0];
        }
        return [code.CompletionItemKind.Text, value];
      }
      function asCompletionItemTag(tag) {
        switch (tag) {
          case ls.CompletionItemTag.Deprecated:
            return code.CompletionItemTag.Deprecated;
        }
        return void 0;
      }
      function asCompletionItemTags(tags) {
        if (tags === void 0 || tags === null) {
          return [];
        }
        const result = [];
        for (const tag of tags) {
          const converted = asCompletionItemTag(tag);
          if (converted !== void 0) {
            result.push(converted);
          }
        }
        return result;
      }
      function asCompletionItem(item, defaultCommitCharacters, commitCharactersApplyKind, defaultRange, defaultInsertTextMode, defaultInsertTextFormat, defaultData, dataApplyKind) {
        const tags = asCompletionItemTags(item.tags);
        const label = asCompletionItemLabel(item);
        const result = new protocolCompletionItem_1.default(label);
        if (item.detail) {
          result.detail = item.detail;
        }
        if (item.documentation) {
          result.documentation = asDocumentation(item.documentation);
          result.documentationFormat = Is2.string(item.documentation) ? "$string" : item.documentation.kind;
        }
        if (item.filterText) {
          result.filterText = item.filterText;
        }
        const insertText = asCompletionInsertText(item, defaultRange, defaultInsertTextFormat);
        if (insertText) {
          result.insertText = insertText.text;
          result.range = insertText.range;
          result.fromEdit = insertText.fromEdit;
        }
        if (Is2.number(item.kind)) {
          const [itemKind, original] = asCompletionItemKind(item.kind);
          result.kind = itemKind;
          if (original) {
            result.originalItemKind = original;
          }
        }
        if (item.sortText) {
          result.sortText = item.sortText;
        }
        if (item.additionalTextEdits) {
          result.additionalTextEdits = asTextEditsSync(item.additionalTextEdits);
        }
        const commitCharacters = applyCommitCharacters(item, defaultCommitCharacters, commitCharactersApplyKind);
        if (commitCharacters) {
          result.commitCharacters = commitCharacters.slice();
        }
        if (item.command) {
          result.command = asCommand(item.command);
        }
        if (item.deprecated === true || item.deprecated === false) {
          result.deprecated = item.deprecated;
          if (item.deprecated === true) {
            tags.push(code.CompletionItemTag.Deprecated);
          }
        }
        if (item.preselect === true || item.preselect === false) {
          result.preselect = item.preselect;
        }
        const data = applyData(item, defaultData, dataApplyKind);
        if (data !== void 0) {
          result.data = data;
        }
        if (tags.length > 0) {
          result.tags = tags;
        }
        const insertTextMode = item.insertTextMode ?? defaultInsertTextMode;
        if (insertTextMode !== void 0) {
          result.insertTextMode = insertTextMode;
          if (insertTextMode === ls.InsertTextMode.asIs) {
            result.keepWhitespace = true;
          }
        }
        return result;
      }
      function applyCommitCharacters(item, defaultCommitCharacters, applyKind) {
        if (applyKind === ls.ApplyKind.Merge) {
          if (!defaultCommitCharacters && !item.commitCharacters) {
            return void 0;
          }
          const set = /* @__PURE__ */ new Set();
          if (defaultCommitCharacters) {
            for (const char of defaultCommitCharacters) {
              set.add(char);
            }
          }
          if (Is2.stringArray(item.commitCharacters)) {
            for (const char of item.commitCharacters) {
              set.add(char);
            }
          }
          return Array.from(set);
        }
        return item.commitCharacters !== void 0 ? Is2.stringArray(item.commitCharacters) ? item.commitCharacters : void 0 : defaultCommitCharacters;
      }
      function applyData(item, defaultData, applyKind) {
        if (applyKind === ls.ApplyKind.Merge) {
          const data = {
            ...defaultData
          };
          if (item.data) {
            Object.entries(item.data).forEach(([key, value]) => {
              if (value !== void 0 && value !== null) {
                data[key] = value;
              }
            });
          }
          return data;
        }
        return item.data ?? defaultData;
      }
      function asCompletionItemLabel(item) {
        if (ls.CompletionItemLabelDetails.is(item.labelDetails)) {
          return {
            label: item.label,
            detail: item.labelDetails.detail,
            description: item.labelDetails.description
          };
        } else {
          return item.label;
        }
      }
      function asCompletionInsertText(item, defaultRange, defaultInsertTextFormat) {
        const insertTextFormat = item.insertTextFormat ?? defaultInsertTextFormat;
        if (item.textEdit !== void 0 || defaultRange !== void 0) {
          const [range, newText] = item.textEdit !== void 0 ? getCompletionRangeAndText(item.textEdit) : [defaultRange, item.textEditText ?? item.label];
          if (insertTextFormat === ls.InsertTextFormat.Snippet) {
            return { text: new code.SnippetString(newText), range, fromEdit: true };
          } else {
            return { text: newText, range, fromEdit: true };
          }
        } else if (item.insertText) {
          if (insertTextFormat === ls.InsertTextFormat.Snippet) {
            return { text: new code.SnippetString(item.insertText), fromEdit: false };
          } else {
            return { text: item.insertText, fromEdit: false };
          }
        } else {
          return void 0;
        }
      }
      function getCompletionRangeAndText(value) {
        if (ls.InsertReplaceEdit.is(value)) {
          return [{ inserting: asRange(value.insert), replacing: asRange(value.replace) }, value.newText];
        } else {
          return [asRange(value.range), value.newText];
        }
      }
      function asTextEdit(edit) {
        if (!edit) {
          return void 0;
        }
        return new code.TextEdit(asRange(edit.range), edit.newText);
      }
      async function asTextEdits(items, token) {
        if (!items) {
          return void 0;
        }
        return async.map(items, asTextEdit, token);
      }
      function asTextEditsSync(items) {
        if (!items) {
          return void 0;
        }
        const result = new Array(items.length);
        for (let i = 0; i < items.length; i++) {
          result[i] = asTextEdit(items[i]);
        }
        return result;
      }
      async function asSignatureHelp(item, token) {
        if (!item) {
          return void 0;
        }
        const result = new code.SignatureHelp();
        if (Is2.number(item.activeSignature)) {
          result.activeSignature = item.activeSignature;
        } else {
          result.activeSignature = 0;
        }
        if (Is2.number(item.activeParameter)) {
          result.activeParameter = item.activeParameter;
        } else if (item.activeParameter === null) {
          result.activeParameter = -1;
        } else {
          result.activeParameter = 0;
        }
        if (item.signatures) {
          result.signatures = await asSignatureInformations(item.signatures, token);
        }
        return result;
      }
      async function asSignatureInformations(items, token) {
        return async.mapAsync(items, asSignatureInformation, token);
      }
      async function asSignatureInformation(item, token) {
        const result = new code.SignatureInformation(item.label);
        if (item.documentation !== void 0) {
          result.documentation = asDocumentation(item.documentation);
        }
        if (item.parameters !== void 0) {
          result.parameters = await asParameterInformations(item.parameters, token);
        }
        if (item.activeParameter !== void 0) {
          result.activeParameter = item.activeParameter ?? -1;
        }
        {
          return result;
        }
      }
      function asParameterInformations(items, token) {
        return async.map(items, asParameterInformation, token);
      }
      function asParameterInformation(item) {
        const result = new code.ParameterInformation(item.label);
        if (item.documentation) {
          result.documentation = asDocumentation(item.documentation);
        }
        return result;
      }
      function asLocation(item) {
        return item ? new code.Location(_uriConverter(item.uri), asRange(item.range)) : void 0;
      }
      async function asDeclarationResult(item, token) {
        if (!item) {
          return void 0;
        }
        return asLocationResult(item, token);
      }
      async function asDefinitionResult(item, token) {
        if (!item) {
          return void 0;
        }
        return asLocationResult(item, token);
      }
      function asLocationLink(item) {
        if (!item) {
          return void 0;
        }
        const result = {
          targetUri: _uriConverter(item.targetUri),
          targetRange: asRange(item.targetRange),
          // See issue: https://github.com/Microsoft/vscode/issues/58649
          originSelectionRange: asRange(item.originSelectionRange),
          targetSelectionRange: asRange(item.targetSelectionRange)
        };
        if (!result.targetSelectionRange) {
          throw new Error(`targetSelectionRange must not be undefined or null`);
        }
        return result;
      }
      async function asLocationResult(item, token) {
        if (!item) {
          return void 0;
        }
        if (Is2.array(item)) {
          if (item.length === 0) {
            return [];
          } else if (ls.LocationLink.is(item[0])) {
            const links = item;
            return async.map(links, asLocationLink, token);
          } else {
            const locations = item;
            return async.map(locations, asLocation, token);
          }
        } else if (ls.LocationLink.is(item)) {
          return [asLocationLink(item)];
        } else {
          return asLocation(item);
        }
      }
      async function asReferences(values, token) {
        if (!values) {
          return void 0;
        }
        return async.map(values, asLocation, token);
      }
      async function asDocumentHighlights(values, token) {
        if (!values) {
          return void 0;
        }
        return async.map(values, asDocumentHighlight, token);
      }
      function asDocumentHighlight(item) {
        const result = new code.DocumentHighlight(asRange(item.range));
        if (Is2.number(item.kind)) {
          result.kind = asDocumentHighlightKind(item.kind);
        }
        return result;
      }
      function asDocumentHighlightKind(item) {
        switch (item) {
          case ls.DocumentHighlightKind.Text:
            return code.DocumentHighlightKind.Text;
          case ls.DocumentHighlightKind.Read:
            return code.DocumentHighlightKind.Read;
          case ls.DocumentHighlightKind.Write:
            return code.DocumentHighlightKind.Write;
        }
        return code.DocumentHighlightKind.Text;
      }
      async function asSymbolInformations(values, token) {
        if (!values) {
          return void 0;
        }
        return async.map(values, asSymbolInformation, token);
      }
      function asSymbolKind(item) {
        if (item <= ls.SymbolKind.TypeParameter) {
          return item - 1;
        }
        return code.SymbolKind.Property;
      }
      function asSymbolTag(value) {
        switch (value) {
          case ls.SymbolTag.Deprecated:
            return code.SymbolTag.Deprecated;
          default:
            return void 0;
        }
      }
      function asSymbolTags(items) {
        if (items === void 0 || items === null) {
          return void 0;
        }
        const result = [];
        for (const item of items) {
          const converted = asSymbolTag(item);
          if (converted !== void 0) {
            result.push(converted);
          }
        }
        return result.length === 0 ? void 0 : result;
      }
      function asSymbolInformation(item) {
        const data = item.data;
        const location = item.location;
        const result = location.range === void 0 || data !== void 0 ? new protocolWorkspaceSymbol_1.default(item.name, asSymbolKind(item.kind), item.containerName ?? "", location.range === void 0 ? _uriConverter(location.uri) : new code.Location(_uriConverter(item.location.uri), asRange(location.range)), data) : new code.SymbolInformation(item.name, asSymbolKind(item.kind), item.containerName ?? "", new code.Location(_uriConverter(item.location.uri), asRange(location.range)));
        fillTags(result, item);
        return result;
      }
      async function asDocumentSymbols(values, token) {
        if (values === void 0 || values === null) {
          return void 0;
        }
        return async.map(values, asDocumentSymbol, token);
      }
      function asDocumentSymbol(value) {
        const result = new code.DocumentSymbol(value.name, value.detail || "", asSymbolKind(value.kind), asRange(value.range), asRange(value.selectionRange));
        fillTags(result, value);
        if (value.children !== void 0 && value.children.length > 0) {
          const children = [];
          for (const child of value.children) {
            children.push(asDocumentSymbol(child));
          }
          result.children = children;
        }
        return result;
      }
      function fillTags(result, value) {
        result.tags = asSymbolTags(value.tags);
        if (value.deprecated) {
          if (!result.tags) {
            result.tags = [code.SymbolTag.Deprecated];
          } else {
            if (!result.tags.includes(code.SymbolTag.Deprecated)) {
              result.tags = result.tags.concat(code.SymbolTag.Deprecated);
            }
          }
        }
      }
      function asCommand(item) {
        const result = { title: item.title, command: item.command };
        if (item.tooltip) {
          result.tooltip = item.tooltip;
        }
        if (item.arguments) {
          result.arguments = item.arguments;
        }
        return result;
      }
      async function asCommands(items, token) {
        if (!items) {
          return void 0;
        }
        return async.map(items, asCommand, token);
      }
      const kindMapping = /* @__PURE__ */ new Map();
      kindMapping.set(ls.CodeActionKind.Empty, code.CodeActionKind.Empty);
      kindMapping.set(ls.CodeActionKind.QuickFix, code.CodeActionKind.QuickFix);
      kindMapping.set(ls.CodeActionKind.Refactor, code.CodeActionKind.Refactor);
      kindMapping.set(ls.CodeActionKind.RefactorExtract, code.CodeActionKind.RefactorExtract);
      kindMapping.set(ls.CodeActionKind.RefactorInline, code.CodeActionKind.RefactorInline);
      kindMapping.set(ls.CodeActionKind.RefactorRewrite, code.CodeActionKind.RefactorRewrite);
      kindMapping.set(ls.CodeActionKind.Source, code.CodeActionKind.Source);
      kindMapping.set(ls.CodeActionKind.SourceOrganizeImports, code.CodeActionKind.SourceOrganizeImports);
      function asCodeActionKind(item) {
        if (item === void 0 || item === null) {
          return void 0;
        }
        let result = kindMapping.get(item);
        if (result) {
          return result;
        }
        const parts = item.split(".");
        result = code.CodeActionKind.Empty;
        for (const part of parts) {
          result = result.append(part);
        }
        return result;
      }
      function asCodeActionKinds(items) {
        if (items === void 0 || items === null) {
          return void 0;
        }
        return items.map((kind) => asCodeActionKind(kind));
      }
      function asCodeActionDocumentations(items) {
        if (items === void 0 || items === null) {
          return void 0;
        }
        return items.map((doc) => ({ kind: asCodeActionKind(doc.kind), command: asCommand(doc.command) }));
      }
      async function asCodeAction(item, token) {
        if (item === void 0 || item === null) {
          return void 0;
        }
        const result = new protocolCodeAction_1.default(item.title, item.data);
        if (item.kind !== void 0) {
          result.kind = asCodeActionKind(item.kind);
        }
        if (item.diagnostics !== void 0) {
          result.diagnostics = asDiagnosticsSync(item.diagnostics);
        }
        if (item.edit !== void 0) {
          result.edit = await asWorkspaceEdit(item.edit, token);
        }
        if (item.command !== void 0) {
          result.command = asCommand(item.command);
        }
        if (item.isPreferred !== void 0) {
          result.isPreferred = item.isPreferred;
        }
        if (item.disabled !== void 0) {
          result.disabled = { reason: item.disabled.reason };
        }
        if (item.tags?.includes(ls.CodeActionTag.LLMGenerated)) {
          result.isAI = true;
        }
        return result;
      }
      function asCodeActionResult(items, token) {
        return async.mapAsync(items, async (item) => {
          if (ls.Command.is(item)) {
            return asCommand(item);
          } else {
            return asCodeAction(item, token);
          }
        }, token);
      }
      function asCodeLens(item) {
        if (!item) {
          return void 0;
        }
        const result = new protocolCodeLens_1.default(asRange(item.range));
        if (item.command) {
          result.command = asCommand(item.command);
        }
        if (item.data !== void 0 && item.data !== null) {
          result.data = item.data;
        }
        return result;
      }
      async function asCodeLenses(items, token) {
        if (!items) {
          return void 0;
        }
        return async.map(items, asCodeLens, token);
      }
      async function asWorkspaceEdit(item, token) {
        if (!item) {
          return void 0;
        }
        const sharedMetadata = /* @__PURE__ */ new Map();
        if (item.changeAnnotations !== void 0) {
          const changeAnnotations = item.changeAnnotations;
          await async.forEach(Object.keys(changeAnnotations), (key) => {
            const metaData = asWorkspaceEditEntryMetadata(changeAnnotations[key]);
            sharedMetadata.set(key, metaData);
          }, token);
        }
        const asMetadata = (annotation) => {
          if (annotation === void 0) {
            return void 0;
          } else {
            return sharedMetadata.get(annotation);
          }
        };
        const result = new code.WorkspaceEdit();
        if (item.documentChanges) {
          const documentChanges = item.documentChanges;
          await async.forEach(documentChanges, (change) => {
            if (ls.CreateFile.is(change)) {
              result.createFile(_uriConverter(change.uri), change.options, asMetadata(change.annotationId));
            } else if (ls.RenameFile.is(change)) {
              result.renameFile(_uriConverter(change.oldUri), _uriConverter(change.newUri), change.options, asMetadata(change.annotationId));
            } else if (ls.DeleteFile.is(change)) {
              result.deleteFile(_uriConverter(change.uri), change.options, asMetadata(change.annotationId));
            } else if (ls.TextDocumentEdit.is(change)) {
              const uri = _uriConverter(change.textDocument.uri);
              const edits = [];
              for (const edit of change.edits) {
                if (ls.AnnotatedTextEdit.is(edit)) {
                  edits.push([new code.TextEdit(asRange(edit.range), edit.newText), asMetadata(edit.annotationId)]);
                } else if (ls.SnippetTextEdit.is(edit)) {
                  edits.push([new code.SnippetTextEdit(asRange(edit.range), new code.SnippetString(edit.snippet.value)), asMetadata(edit.annotationId)]);
                } else {
                  edits.push([new code.TextEdit(asRange(edit.range), edit.newText), void 0]);
                }
              }
              result.set(uri, edits);
            } else {
              throw new Error(`Unknown workspace edit change received:
${JSON.stringify(change, void 0, 4)}`);
            }
          }, token);
        } else if (item.changes) {
          const changes = item.changes;
          await async.forEach(Object.keys(changes), (key) => {
            result.set(_uriConverter(key), asTextEditsSync(changes[key]));
          }, token);
        }
        return result;
      }
      function asWorkspaceEditEntryMetadata(annotation) {
        if (annotation === void 0) {
          return void 0;
        }
        return { label: annotation.label, needsConfirmation: !!annotation.needsConfirmation, description: annotation.description };
      }
      function asDocumentLink(item) {
        const range = asRange(item.range);
        const target = item.target ? asUri(item.target) : void 0;
        const link = new protocolDocumentLink_1.default(range, target);
        if (item.tooltip !== void 0) {
          link.tooltip = item.tooltip;
        }
        if (item.data !== void 0 && item.data !== null) {
          link.data = item.data;
        }
        return link;
      }
      async function asDocumentLinks(items, token) {
        if (!items) {
          return void 0;
        }
        return async.map(items, asDocumentLink, token);
      }
      function asColor(color) {
        return new code.Color(color.red, color.green, color.blue, color.alpha);
      }
      function asColorInformation(ci) {
        return new code.ColorInformation(asRange(ci.range), asColor(ci.color));
      }
      async function asColorInformations(colorInformation, token) {
        if (!colorInformation) {
          return void 0;
        }
        return async.map(colorInformation, asColorInformation, token);
      }
      function asColorPresentation(cp) {
        const presentation = new code.ColorPresentation(cp.label);
        presentation.additionalTextEdits = asTextEditsSync(cp.additionalTextEdits);
        if (cp.textEdit) {
          presentation.textEdit = asTextEdit(cp.textEdit);
        }
        return presentation;
      }
      async function asColorPresentations(colorPresentations, token) {
        if (!colorPresentations) {
          return void 0;
        }
        return async.map(colorPresentations, asColorPresentation, token);
      }
      function asFoldingRangeKind(kind) {
        if (kind) {
          switch (kind) {
            case ls.FoldingRangeKind.Comment:
              return code.FoldingRangeKind.Comment;
            case ls.FoldingRangeKind.Imports:
              return code.FoldingRangeKind.Imports;
            case ls.FoldingRangeKind.Region:
              return code.FoldingRangeKind.Region;
          }
        }
        return void 0;
      }
      function asFoldingRange(r) {
        return new code.FoldingRange(r.startLine, r.endLine, asFoldingRangeKind(r.kind));
      }
      async function asFoldingRanges(foldingRanges, token) {
        if (!foldingRanges) {
          return void 0;
        }
        return async.map(foldingRanges, asFoldingRange, token);
      }
      function asSelectionRange(selectionRange) {
        return new code.SelectionRange(asRange(selectionRange.range), selectionRange.parent ? asSelectionRange(selectionRange.parent) : void 0);
      }
      async function asSelectionRanges(selectionRanges, token) {
        if (!Array.isArray(selectionRanges)) {
          return [];
        }
        return async.map(selectionRanges, asSelectionRange, token);
      }
      function asInlineValue(inlineValue) {
        if (ls.InlineValueText.is(inlineValue)) {
          return new code.InlineValueText(asRange(inlineValue.range), inlineValue.text);
        } else if (ls.InlineValueVariableLookup.is(inlineValue)) {
          return new code.InlineValueVariableLookup(asRange(inlineValue.range), inlineValue.variableName, inlineValue.caseSensitiveLookup);
        } else {
          return new code.InlineValueEvaluatableExpression(asRange(inlineValue.range), inlineValue.expression);
        }
      }
      async function asInlineValues(inlineValues, token) {
        if (!Array.isArray(inlineValues)) {
          return [];
        }
        return async.map(inlineValues, asInlineValue, token);
      }
      async function asInlayHint(value, token) {
        const label = typeof value.label === "string" ? value.label : await async.map(value.label, asInlayHintLabelPart, token);
        const result = new protocolInlayHint_1.default(asPosition(value.position), label);
        if (value.kind !== void 0) {
          result.kind = value.kind;
        }
        if (value.textEdits !== void 0) {
          result.textEdits = await asTextEdits(value.textEdits, token);
        }
        if (value.tooltip !== void 0) {
          result.tooltip = asTooltip(value.tooltip);
        }
        if (value.paddingLeft !== void 0) {
          result.paddingLeft = value.paddingLeft;
        }
        if (value.paddingRight !== void 0) {
          result.paddingRight = value.paddingRight;
        }
        if (value.data !== void 0) {
          result.data = value.data;
        }
        return result;
      }
      function asInlayHintLabelPart(part) {
        const result = new code.InlayHintLabelPart(part.value);
        if (part.location !== void 0) {
          result.location = asLocation(part.location);
        }
        if (part.tooltip !== void 0) {
          result.tooltip = asTooltip(part.tooltip);
        }
        if (part.command !== void 0) {
          result.command = asCommand(part.command);
        }
        return result;
      }
      function asTooltip(value) {
        if (typeof value === "string") {
          return value;
        }
        return asMarkdownString(value);
      }
      async function asInlayHints(values, token) {
        if (!Array.isArray(values)) {
          return void 0;
        }
        return async.mapAsync(values, asInlayHint, token);
      }
      function asCallHierarchyItem(item) {
        if (item === null) {
          return void 0;
        }
        const result = new protocolCallHierarchyItem_1.default(asSymbolKind(item.kind), item.name, item.detail || "", asUri(item.uri), asRange(item.range), asRange(item.selectionRange), item.data);
        if (item.tags !== void 0) {
          result.tags = asSymbolTags(item.tags);
        }
        return result;
      }
      async function asCallHierarchyItems(items, token) {
        if (items === null) {
          return void 0;
        }
        return async.map(items, asCallHierarchyItem, token);
      }
      async function asCallHierarchyIncomingCall(item, token) {
        return new code.CallHierarchyIncomingCall(asCallHierarchyItem(item.from), await asRanges(item.fromRanges, token));
      }
      async function asCallHierarchyIncomingCalls(items, token) {
        if (items === null) {
          return void 0;
        }
        return async.mapAsync(items, asCallHierarchyIncomingCall, token);
      }
      async function asCallHierarchyOutgoingCall(item, token) {
        return new code.CallHierarchyOutgoingCall(asCallHierarchyItem(item.to), await asRanges(item.fromRanges, token));
      }
      async function asCallHierarchyOutgoingCalls(items, token) {
        if (items === null) {
          return void 0;
        }
        return async.mapAsync(items, asCallHierarchyOutgoingCall, token);
      }
      async function asSemanticTokens(value, _token) {
        if (value === void 0 || value === null) {
          return void 0;
        }
        return new code.SemanticTokens(new Uint32Array(value.data), value.resultId);
      }
      function asSemanticTokensEdit(value) {
        return new code.SemanticTokensEdit(value.start, value.deleteCount, value.data !== void 0 ? new Uint32Array(value.data) : void 0);
      }
      async function asSemanticTokensEdits(value, _token) {
        if (value === void 0 || value === null) {
          return void 0;
        }
        return new code.SemanticTokensEdits(value.edits.map(asSemanticTokensEdit), value.resultId);
      }
      function asSemanticTokensLegend(value) {
        return value;
      }
      async function asLinkedEditingRanges(value, token) {
        if (value === null || value === void 0) {
          return void 0;
        }
        return new code.LinkedEditingRanges(await asRanges(value.ranges, token), asRegularExpression(value.wordPattern));
      }
      function asRegularExpression(value) {
        if (value === null || value === void 0) {
          return void 0;
        }
        return new RegExp(value);
      }
      function asTypeHierarchyItem(item) {
        if (item === null) {
          return void 0;
        }
        const result = new protocolTypeHierarchyItem_1.default(asSymbolKind(item.kind), item.name, item.detail || "", asUri(item.uri), asRange(item.range), asRange(item.selectionRange), item.data);
        if (item.tags !== void 0) {
          result.tags = asSymbolTags(item.tags);
        }
        return result;
      }
      async function asTypeHierarchyItems(items, token) {
        if (items === null) {
          return void 0;
        }
        return async.map(items, asTypeHierarchyItem, token);
      }
      function asGlobPattern(pattern) {
        if (Is2.string(pattern)) {
          return pattern;
        }
        if (ls.RelativePattern.is(pattern)) {
          if (ls.URI.is(pattern.baseUri)) {
            return new code.RelativePattern(asUri(pattern.baseUri), pattern.pattern);
          } else if (ls.WorkspaceFolder.is(pattern.baseUri)) {
            const workspaceFolder = code.workspace.getWorkspaceFolder(asUri(pattern.baseUri.uri));
            return workspaceFolder !== void 0 ? new code.RelativePattern(workspaceFolder, pattern.pattern) : void 0;
          }
        }
        return void 0;
      }
      async function asInlineCompletionResult(value, token) {
        if (!value) {
          return void 0;
        }
        if (Array.isArray(value)) {
          return async.map(value, (item) => asInlineCompletionItem(item), token);
        }
        const list = value;
        const converted = await async.map(list.items, (item) => {
          return asInlineCompletionItem(item);
        }, token);
        return new code.InlineCompletionList(converted);
      }
      function asInlineCompletionItem(item) {
        let insertText;
        if (typeof item.insertText === "string") {
          insertText = item.insertText;
        } else {
          insertText = new code.SnippetString(item.insertText.value);
        }
        let command = void 0;
        if (item.command) {
          command = asCommand(item.command);
        }
        const inlineCompletionItem = new code.InlineCompletionItem(insertText, asRange(item.range), command);
        if (item.filterText) {
          inlineCompletionItem.filterText = item.filterText;
        }
        return inlineCompletionItem;
      }
      return {
        asUri,
        asDocumentSelector,
        asDiagnostics,
        asDiagnostic,
        asRange,
        asRanges,
        asPosition,
        asDiagnosticSeverity,
        asDiagnosticTag,
        asHover,
        asCompletionResult,
        asCompletionItem,
        asTextEdit,
        asTextEdits,
        asSignatureHelp,
        asSignatureInformations,
        asSignatureInformation,
        asParameterInformations,
        asParameterInformation,
        asDeclarationResult,
        asDefinitionResult,
        asLocation,
        asReferences,
        asDocumentHighlights,
        asDocumentHighlight,
        asDocumentHighlightKind,
        asSymbolKind,
        asSymbolTag,
        asSymbolTags,
        asSymbolInformations,
        asSymbolInformation,
        asDocumentSymbols,
        asDocumentSymbol,
        asCommand,
        asCommands,
        asCodeAction,
        asCodeActionKind,
        asCodeActionKinds,
        asCodeActionDocumentations,
        asCodeActionResult,
        asCodeLens,
        asCodeLenses,
        asWorkspaceEdit,
        asDocumentLink,
        asDocumentLinks,
        asFoldingRangeKind,
        asFoldingRange,
        asFoldingRanges,
        asColor,
        asColorInformation,
        asColorInformations,
        asColorPresentation,
        asColorPresentations,
        asSelectionRange,
        asSelectionRanges,
        asInlineValue,
        asInlineValues,
        asInlayHint,
        asInlayHints,
        asSemanticTokensLegend,
        asSemanticTokens,
        asSemanticTokensEdit,
        asSemanticTokensEdits,
        asCallHierarchyItem,
        asCallHierarchyItems,
        asCallHierarchyIncomingCall,
        asCallHierarchyIncomingCalls,
        asCallHierarchyOutgoingCall,
        asCallHierarchyOutgoingCalls,
        asLinkedEditingRanges,
        asTypeHierarchyItem,
        asTypeHierarchyItems,
        asGlobPattern,
        asInlineCompletionResult,
        asInlineCompletionItem
      };
    }
  }
});

// node_modules/vscode-languageclient/lib/common/utils/uuid.js
var require_uuid = __commonJS({
  "node_modules/vscode-languageclient/lib/common/utils/uuid.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.empty = void 0;
    exports2.v4 = v4;
    exports2.isUUID = isUUID;
    exports2.parse = parse;
    exports2.generateUuid = generateUuid;
    var ValueUUID = class {
      _value;
      constructor(_value) {
        this._value = _value;
      }
      asHex() {
        return this._value;
      }
      equals(other) {
        return this.asHex() === other.asHex();
      }
    };
    var V4UUID = class _V4UUID extends ValueUUID {
      static _chars = ["0", "1", "2", "3", "4", "5", "6", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f"];
      static _timeHighBits = ["8", "9", "a", "b"];
      static _oneOf(array) {
        return array[Math.floor(array.length * Math.random())];
      }
      static _randomHex() {
        return _V4UUID._oneOf(_V4UUID._chars);
      }
      constructor() {
        super([
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          "-",
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          "-",
          "4",
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          "-",
          _V4UUID._oneOf(_V4UUID._timeHighBits),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          "-",
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex(),
          _V4UUID._randomHex()
        ].join(""));
      }
    };
    exports2.empty = new ValueUUID("00000000-0000-0000-0000-000000000000");
    function v4() {
      return new V4UUID();
    }
    var _UUIDPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    function isUUID(value) {
      return _UUIDPattern.test(value);
    }
    function parse(value) {
      if (!isUUID(value)) {
        throw new Error("invalid uuid");
      }
      return new ValueUUID(value);
    }
    function generateUuid() {
      return v4().asHex();
    }
  }
});

// node_modules/vscode-languageclient/lib/common/progressPart.js
var require_progressPart = __commonJS({
  "node_modules/vscode-languageclient/lib/common/progressPart.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ProgressPart = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var Is2 = __importStar(require_is());
    var ProgressPart = class {
      _client;
      _token;
      _infinite;
      _reported;
      // listener for LSP progress messages. Set in constructor.
      _lspProgressDisposable;
      // VS Code progress state. Set in Window.withProgress callback.
      _progress;
      _cancellationToken;
      _tokenDisposable;
      _resolve;
      _reject;
      constructor(_client, _token, done) {
        this._client = _client;
        this._token = _token;
        this._reported = 0;
        this._infinite = false;
        this._lspProgressDisposable = this._client.onProgress(vscode_languageserver_protocol_1.WorkDoneProgress.type, this._token, (value) => {
          switch (value.kind) {
            case "begin":
              this.begin(value);
              break;
            case "report":
              this.report(value);
              break;
            case "end":
              this.done();
              done && done(this);
              break;
          }
        });
      }
      begin(params) {
        this._infinite = params.percentage === void 0;
        if (this._lspProgressDisposable === void 0) {
          return;
        }
        void vscode_1.window.withProgress({ location: vscode_1.ProgressLocation.Window, cancellable: params.cancellable, title: params.title }, async (progress, cancellationToken) => {
          if (this._lspProgressDisposable === void 0) {
            return;
          }
          this._progress = progress;
          this._cancellationToken = cancellationToken;
          this._tokenDisposable = this._cancellationToken.onCancellationRequested(() => {
            this._client.sendNotification(vscode_languageserver_protocol_1.WorkDoneProgressCancelNotification.type, { token: this._token });
          });
          this.report(params);
          return new Promise((resolve, reject) => {
            this._resolve = resolve;
            this._reject = reject;
          });
        });
      }
      report(params) {
        if (this._infinite && Is2.string(params.message)) {
          this._progress !== void 0 && this._progress.report({ message: params.message });
        } else if (Is2.number(params.percentage)) {
          const percentage = Math.max(0, Math.min(params.percentage, 100));
          const delta = Math.max(0, percentage - this._reported);
          this._reported += delta;
          this._progress !== void 0 && this._progress.report({ message: params.message, increment: delta });
        }
      }
      cancel() {
        this.cleanup();
        if (this._reject !== void 0) {
          this._reject();
          this._resolve = void 0;
          this._reject = void 0;
        }
      }
      done() {
        this.cleanup();
        if (this._resolve !== void 0) {
          this._resolve();
          this._resolve = void 0;
          this._reject = void 0;
        }
      }
      cleanup() {
        if (this._lspProgressDisposable !== void 0) {
          this._lspProgressDisposable.dispose();
          this._lspProgressDisposable = void 0;
        }
        if (this._tokenDisposable !== void 0) {
          this._tokenDisposable.dispose();
          this._tokenDisposable = void 0;
        }
        this._progress = void 0;
        this._cancellationToken = void 0;
      }
    };
    exports2.ProgressPart = ProgressPart;
  }
});

// node_modules/vscode-languageclient/lib/common/features.js
var require_features = __commonJS({
  "node_modules/vscode-languageclient/lib/common/features.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DefaultDiagnosticCollectionProvider = exports2.DiagnosticCollectionSource = exports2.WorkspaceFeature = exports2.TextDocumentLanguageFeature = exports2.TextDocumentEventFeature = exports2.DynamicDocumentFeature = exports2.DynamicFeature = exports2.StaticFeature = exports2.LSPCancellationError = void 0;
    exports2.ensure = ensure;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var Is2 = __importStar(require_is());
    var UUID = __importStar(require_uuid());
    var LSPCancellationError = class extends vscode_1.CancellationError {
      data;
      constructor(data) {
        super();
        this.data = data;
      }
    };
    exports2.LSPCancellationError = LSPCancellationError;
    function ensure(target, key) {
      if (target[key] === void 0) {
        target[key] = {};
      }
      return target[key];
    }
    var StaticFeature;
    (function(StaticFeature2) {
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && Is2.func(candidate.fillClientCapabilities) && Is2.func(candidate.initialize) && Is2.func(candidate.getState) && Is2.func(candidate.clear) && (candidate.fillInitializeParams === void 0 || Is2.func(candidate.fillInitializeParams));
      }
      StaticFeature2.is = is;
    })(StaticFeature || (exports2.StaticFeature = StaticFeature = {}));
    var DynamicFeature;
    (function(DynamicFeature2) {
      function is(value) {
        const candidate = value;
        return candidate !== void 0 && candidate !== null && Is2.func(candidate.fillClientCapabilities) && Is2.func(candidate.initialize) && Is2.func(candidate.getState) && Is2.func(candidate.clear) && (candidate.fillInitializeParams === void 0 || Is2.func(candidate.fillInitializeParams)) && Is2.func(candidate.register) && Is2.func(candidate.unregister) && candidate.registrationType !== void 0;
      }
      DynamicFeature2.is = is;
    })(DynamicFeature || (exports2.DynamicFeature = DynamicFeature = {}));
    var DynamicDocumentFeature = class {
      _client;
      constructor(client) {
        this._client = client;
      }
      /**
       * Returns the state the feature is in.
       */
      getState() {
        const selectors = this.getDocumentSelectors();
        let count = 0;
        for (const selector of selectors) {
          count++;
          for (const document of vscode_1.workspace.textDocuments) {
            if (vscode_1.languages.match(selector, document) > 0) {
              return { kind: "document", id: this.registrationType.method, registrations: true, matches: true };
            }
          }
        }
        const registrations = count > 0;
        return { kind: "document", id: this.registrationType.method, registrations, matches: false };
      }
    };
    exports2.DynamicDocumentFeature = DynamicDocumentFeature;
    var TextDocumentEventFeature = class extends DynamicDocumentFeature {
      _event;
      _type;
      _middleware;
      _createParams;
      _textDocument;
      _selectorFilter;
      _listener;
      _selectors;
      _onAboutToSendNotification;
      _onNotificationSent;
      static textDocumentFilter(selectors, textDocument) {
        for (const selector of selectors) {
          if (vscode_1.languages.match(selector, textDocument) > 0) {
            return true;
          }
        }
        return false;
      }
      constructor(client, event, type, middleware, createParams, textDocument, selectorFilter) {
        super(client);
        this._event = event;
        this._type = type;
        this._middleware = middleware;
        this._createParams = createParams;
        this._textDocument = textDocument;
        this._selectorFilter = selectorFilter;
        this._selectors = /* @__PURE__ */ new Map();
        this._onAboutToSendNotification = new vscode_1.EventEmitter();
        this._onNotificationSent = new vscode_1.EventEmitter();
      }
      getStateInfo() {
        return [this._selectors.values(), false];
      }
      getDocumentSelectors() {
        return this._selectors.values();
      }
      register(data) {
        if (!data.registerOptions.documentSelector) {
          return;
        }
        if (!this._listener) {
          this._listener = this._event((data2) => {
            this.callback(data2).catch((error) => {
              this._client.error(`Sending document notification ${this._type.method} failed.`, error);
            });
          });
        }
        this._selectors.set(data.id, this._client.protocol2CodeConverter.asDocumentSelector(data.registerOptions.documentSelector));
      }
      async callback(data) {
        const doSend = async (data2) => {
          const textDocument = this.getTextDocument(data2);
          const params = this._createParams(data2);
          this.aboutToSendNotification(textDocument, this._type, params);
          await this._client.sendNotification(this._type, params);
          this.notificationSent(textDocument, this._type, params);
        };
        if (this.matches(data)) {
          const middleware = this._middleware();
          return middleware ? middleware(data, (data2) => doSend(data2)) : doSend(data);
        }
      }
      matches(data) {
        if (this._client.hasDedicatedTextSynchronizationFeature(this._textDocument(data))) {
          return false;
        }
        return !this._selectorFilter || this._selectorFilter(this._selectors.values(), data);
      }
      get onAboutToSendNotification() {
        return this._onAboutToSendNotification.event;
      }
      aboutToSendNotification(textDocument, type, params) {
        this._onAboutToSendNotification.fire({ textDocument, type, params });
      }
      get onNotificationSent() {
        return this._onNotificationSent.event;
      }
      notificationSent(textDocument, type, params) {
        this._onNotificationSent.fire({ textDocument, type, params });
      }
      unregister(id) {
        this._selectors.delete(id);
        if (this._selectors.size === 0 && this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      clear() {
        this._selectors.clear();
        this._onNotificationSent.dispose();
        this._onNotificationSent = new vscode_1.EventEmitter();
        if (this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      getProvider(document) {
        for (const selector of this._selectors.values()) {
          if (vscode_1.languages.match(selector, document) > 0) {
            return {
              send: (data) => {
                return this.callback(data);
              }
            };
          }
        }
        return void 0;
      }
    };
    exports2.TextDocumentEventFeature = TextDocumentEventFeature;
    var TextDocumentLanguageFeature = class extends DynamicDocumentFeature {
      _registrationType;
      _registrations;
      constructor(client, registrationType) {
        super(client);
        this._registrationType = registrationType;
        this._registrations = /* @__PURE__ */ new Map();
      }
      *getDocumentSelectors() {
        for (const registration of this._registrations.values()) {
          const selector = registration.data.registerOptions.documentSelector;
          if (selector === null) {
            continue;
          }
          yield this._client.protocol2CodeConverter.asDocumentSelector(selector);
        }
      }
      get registrationType() {
        return this._registrationType;
      }
      register(data) {
        if (!data.registerOptions.documentSelector) {
          return;
        }
        const registration = this.registerLanguageProvider(data.registerOptions, data.id);
        this._registrations.set(data.id, { disposable: registration[0], data, provider: registration[1] });
      }
      unregister(id) {
        const registration = this._registrations.get(id);
        if (registration !== void 0) {
          this._registrations.delete(id);
          registration.disposable.dispose();
        }
      }
      clear() {
        this._registrations.forEach((value) => {
          value.disposable.dispose();
        });
        this._registrations.clear();
      }
      getRegistration(documentSelector, capability) {
        if (!capability) {
          return [void 0, void 0];
        } else if (vscode_languageserver_protocol_1.TextDocumentRegistrationOptions.is(capability)) {
          const id = vscode_languageserver_protocol_1.StaticRegistrationOptions.hasId(capability) ? capability.id : UUID.generateUuid();
          const selector = capability.documentSelector ?? documentSelector;
          if (selector) {
            return [id, Object.assign({}, capability, { documentSelector: selector })];
          }
        } else if (Is2.boolean(capability) && capability === true || vscode_languageserver_protocol_1.WorkDoneProgressOptions.is(capability)) {
          if (!documentSelector) {
            return [void 0, void 0];
          }
          const options = Is2.boolean(capability) && capability === true ? { documentSelector } : Object.assign({}, capability, { documentSelector });
          return [UUID.generateUuid(), options];
        }
        return [void 0, void 0];
      }
      getRegistrationOptions(documentSelector, capability) {
        if (!documentSelector || !capability) {
          return void 0;
        }
        return Is2.boolean(capability) && capability === true ? { documentSelector } : Object.assign({}, capability, { documentSelector });
      }
      getProvider(textDocument) {
        for (const registration of this._registrations.values()) {
          const selector = registration.data.registerOptions.documentSelector;
          if (selector !== null && vscode_1.languages.match(this._client.protocol2CodeConverter.asDocumentSelector(selector), textDocument) > 0) {
            return registration.provider;
          }
        }
        return void 0;
      }
      getAllProviders() {
        const result = [];
        for (const item of this._registrations.values()) {
          result.push(item.provider);
        }
        return result;
      }
    };
    exports2.TextDocumentLanguageFeature = TextDocumentLanguageFeature;
    var WorkspaceFeature = class {
      _client;
      _registrationType;
      _registrations;
      constructor(client, registrationType) {
        this._client = client;
        this._registrationType = registrationType;
        this._registrations = /* @__PURE__ */ new Map();
      }
      getState() {
        const registrations = this._registrations.size > 0;
        return { kind: "workspace", id: this._registrationType.method, registrations };
      }
      get registrationType() {
        return this._registrationType;
      }
      register(data) {
        const registration = this.registerLanguageProvider(data.registerOptions);
        this._registrations.set(data.id, { disposable: registration[0], provider: registration[1] });
      }
      unregister(id) {
        const registration = this._registrations.get(id);
        if (registration !== void 0) {
          this._registrations.delete(id);
          registration.disposable.dispose();
        }
      }
      clear() {
        this._registrations.forEach((registration) => {
          registration.disposable.dispose();
        });
        this._registrations.clear();
      }
      getProviders() {
        const result = [];
        for (const registration of this._registrations.values()) {
          result.push(registration.provider);
        }
        return result;
      }
    };
    exports2.WorkspaceFeature = WorkspaceFeature;
    var DiagnosticCollectionSource;
    (function(DiagnosticCollectionSource2) {
      DiagnosticCollectionSource2["push"] = "push";
      DiagnosticCollectionSource2["pull"] = "pull";
    })(DiagnosticCollectionSource || (exports2.DiagnosticCollectionSource = DiagnosticCollectionSource = {}));
    var DefaultDiagnosticCollectionProvider = class {
      create(name, _source) {
        return name !== void 0 ? vscode_1.languages.createDiagnosticCollection(name) : vscode_1.languages.createDiagnosticCollection();
      }
      dispose(collection, _source) {
        collection.dispose();
      }
    };
    exports2.DefaultDiagnosticCollectionProvider = DefaultDiagnosticCollectionProvider;
  }
});

// node_modules/balanced-match/dist/commonjs/index.js
var require_commonjs = __commonJS({
  "node_modules/balanced-match/dist/commonjs/index.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.range = exports2.balanced = void 0;
    var balanced = (a, b, str) => {
      const ma = a instanceof RegExp ? maybeMatch(a, str) : a;
      const mb = b instanceof RegExp ? maybeMatch(b, str) : b;
      const r = ma !== null && mb != null && (0, exports2.range)(ma, mb, str);
      return r && {
        start: r[0],
        end: r[1],
        pre: str.slice(0, r[0]),
        body: str.slice(r[0] + ma.length, r[1]),
        post: str.slice(r[1] + mb.length)
      };
    };
    exports2.balanced = balanced;
    var maybeMatch = (reg, str) => {
      const m = str.match(reg);
      return m ? m[0] : null;
    };
    var range = (a, b, str) => {
      let begs, beg, left, right = void 0, result;
      let ai = str.indexOf(a);
      let bi = str.indexOf(b, ai + 1);
      let i = ai;
      if (ai >= 0 && bi > 0) {
        if (a === b) {
          return [ai, bi];
        }
        begs = [];
        left = str.length;
        while (i >= 0 && !result) {
          if (i === ai) {
            begs.push(i);
            ai = str.indexOf(a, i + 1);
          } else if (begs.length === 1) {
            const r = begs.pop();
            if (r !== void 0)
              result = [r, bi];
          } else {
            beg = begs.pop();
            if (beg !== void 0 && beg < left) {
              left = beg;
              right = bi;
            }
            bi = str.indexOf(b, i + 1);
          }
          i = ai < bi && ai >= 0 ? ai : bi;
        }
        if (begs.length && right !== void 0) {
          result = [left, right];
        }
      }
      return result;
    };
    exports2.range = range;
  }
});

// node_modules/brace-expansion/dist/commonjs/index.js
var require_commonjs2 = __commonJS({
  "node_modules/brace-expansion/dist/commonjs/index.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.EXPANSION_MAX_LENGTH = exports2.EXPANSION_MAX = void 0;
    exports2.expand = expand;
    var balanced_match_1 = require_commonjs();
    var escSlash = "\0SLASH" + Math.random() + "\0";
    var escOpen = "\0OPEN" + Math.random() + "\0";
    var escClose = "\0CLOSE" + Math.random() + "\0";
    var escComma = "\0COMMA" + Math.random() + "\0";
    var escPeriod = "\0PERIOD" + Math.random() + "\0";
    var escSlashPattern = new RegExp(escSlash, "g");
    var escOpenPattern = new RegExp(escOpen, "g");
    var escClosePattern = new RegExp(escClose, "g");
    var escCommaPattern = new RegExp(escComma, "g");
    var escPeriodPattern = new RegExp(escPeriod, "g");
    var slashPattern = /\\\\/g;
    var openPattern = /\\{/g;
    var closePattern = /\\}/g;
    var commaPattern = /\\,/g;
    var periodPattern = /\\\./g;
    exports2.EXPANSION_MAX = 1e5;
    exports2.EXPANSION_MAX_LENGTH = 4e6;
    function numeric(str) {
      return !isNaN(str) ? parseInt(str, 10) : str.charCodeAt(0);
    }
    function escapeBraces(str) {
      return str.replace(slashPattern, escSlash).replace(openPattern, escOpen).replace(closePattern, escClose).replace(commaPattern, escComma).replace(periodPattern, escPeriod);
    }
    function unescapeBraces(str) {
      return str.replace(escSlashPattern, "\\").replace(escOpenPattern, "{").replace(escClosePattern, "}").replace(escCommaPattern, ",").replace(escPeriodPattern, ".");
    }
    function parseCommaParts(str) {
      if (!str) {
        return [""];
      }
      const parts = [];
      const m = (0, balanced_match_1.balanced)("{", "}", str);
      if (!m) {
        return str.split(",");
      }
      const { pre, body, post } = m;
      const p = pre.split(",");
      p[p.length - 1] += "{" + body + "}";
      const postParts = parseCommaParts(post);
      if (post.length) {
        ;
        p[p.length - 1] += postParts.shift();
        p.push.apply(p, postParts);
      }
      parts.push.apply(parts, p);
      return parts;
    }
    function expand(str, options = {}) {
      if (!str) {
        return [];
      }
      const { max = exports2.EXPANSION_MAX, maxLength = exports2.EXPANSION_MAX_LENGTH } = options;
      if (str.slice(0, 2) === "{}") {
        str = "\\{\\}" + str.slice(2);
      }
      return expand_(escapeBraces(str), max, maxLength, true).map(unescapeBraces);
    }
    function embrace(str) {
      return "{" + str + "}";
    }
    function isPadded(el) {
      return /^-?0\d/.test(el);
    }
    function lte(i, y) {
      return i <= y;
    }
    function gte(i, y) {
      return i >= y;
    }
    function combine(acc, pre, values, max, maxLength, dropEmpties) {
      const out = [];
      let length = 0;
      for (let a = 0; a < acc.length; a++) {
        for (let v = 0; v < values.length; v++) {
          if (out.length >= max)
            return out;
          const expansion = acc[a] + pre + values[v];
          if (dropEmpties && !expansion)
            continue;
          if (length + expansion.length > maxLength)
            return out;
          out.push(expansion);
          length += expansion.length;
        }
      }
      return out;
    }
    function expandSequence(body, isAlphaSequence, max, maxLength) {
      const n = body.split(/\.\./);
      const N = [];
      if (n[0] === void 0 || n[1] === void 0) {
        return N;
      }
      const x = numeric(n[0]);
      const y = numeric(n[1]);
      const width = Math.max(n[0].length, n[1].length);
      let incr = n.length === 3 && n[2] !== void 0 ? Math.max(Math.abs(numeric(n[2])), 1) : 1;
      let test = lte;
      const reverse = y < x;
      if (reverse) {
        incr *= -1;
        test = gte;
      }
      const pad = n.some(isPadded);
      let length = 0;
      for (let i = x; test(i, y) && N.length < max; i += incr) {
        let c;
        if (isAlphaSequence) {
          c = String.fromCharCode(i);
          if (c === "\\") {
            c = "";
          }
        } else {
          c = String(i);
          if (pad) {
            const need = width - c.length;
            if (need > 0) {
              const z = new Array(need + 1).join("0");
              if (i < 0) {
                c = "-" + z + c.slice(1);
              } else {
                c = z + c;
              }
            }
          }
        }
        if (length + c.length > maxLength)
          break;
        N.push(c);
        length += c.length;
      }
      return N;
    }
    function expand_(str, max, maxLength, isTop) {
      let acc = [""];
      let dropEmpties = false;
      let firstGroup = true;
      for (; ; ) {
        const m = (0, balanced_match_1.balanced)("{", "}", str);
        if (!m) {
          return combine(acc, str, [""], max, maxLength, dropEmpties);
        }
        const pre = m.pre;
        if (/\$$/.test(pre)) {
          acc = combine(acc, pre + "{" + m.body + "}", [""], max, maxLength, dropEmpties && !m.post.length);
          firstGroup = false;
          if (!m.post.length)
            break;
          str = m.post;
          continue;
        }
        const isNumericSequence = /^-?\d+\.\.-?\d+(?:\.\.-?\d+)?$/.test(m.body);
        const isAlphaSequence = /^[a-zA-Z]\.\.[a-zA-Z](?:\.\.-?\d+)?$/.test(m.body);
        const isSequence = isNumericSequence || isAlphaSequence;
        const isOptions = m.body.indexOf(",") >= 0;
        if (!isSequence && !isOptions) {
          if (m.post.match(/,(?!,).*\}/)) {
            str = m.pre + "{" + m.body + escClose + m.post;
            isTop = true;
            continue;
          }
          return combine(acc, pre + "{" + m.body + "}" + m.post, [""], max, maxLength, dropEmpties);
        }
        if (firstGroup) {
          dropEmpties = isTop && !isSequence;
          firstGroup = false;
        }
        let values;
        if (isSequence) {
          values = expandSequence(m.body, isAlphaSequence, max, maxLength);
        } else {
          let n = parseCommaParts(m.body);
          if (n.length === 1 && n[0] !== void 0) {
            n = expand_(n[0], max, maxLength, false).map(embrace);
            if (n.length === 1) {
              acc = combine(acc, pre + n[0], [""], max, maxLength, dropEmpties && !m.post.length);
              if (!m.post.length)
                break;
              str = m.post;
              continue;
            }
          }
          let dropsEmpties = dropEmpties && !m.post.length && !pre;
          for (let d = 0; dropsEmpties && d < acc.length; d++) {
            if (acc[d]) {
              dropsEmpties = false;
            }
          }
          values = [];
          let valuesLength = 0;
          outer: for (let j = 0; j < n.length; j++) {
            const expanded = expand_(n[j], max, maxLength, false);
            for (let k = 0; k < expanded.length; k++) {
              const v = expanded[k];
              if (dropsEmpties && !v)
                continue;
              if (values.length >= max || valuesLength + v.length > maxLength) {
                break outer;
              }
              values.push(v);
              valuesLength += v.length;
            }
          }
        }
        acc = combine(acc, pre, values, max, maxLength, dropEmpties && !m.post.length);
        if (!m.post.length)
          break;
        str = m.post;
      }
      return acc;
    }
  }
});

// node_modules/minimatch/dist/commonjs/assert-valid-pattern.js
var require_assert_valid_pattern = __commonJS({
  "node_modules/minimatch/dist/commonjs/assert-valid-pattern.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.assertValidPattern = void 0;
    var MAX_PATTERN_LENGTH = 1024 * 64;
    var assertValidPattern = (pattern) => {
      if (typeof pattern !== "string") {
        throw new TypeError("invalid pattern");
      }
      if (pattern.length > MAX_PATTERN_LENGTH) {
        throw new TypeError("pattern is too long");
      }
    };
    exports2.assertValidPattern = assertValidPattern;
  }
});

// node_modules/minimatch/dist/commonjs/brace-expressions.js
var require_brace_expressions = __commonJS({
  "node_modules/minimatch/dist/commonjs/brace-expressions.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.parseClass = void 0;
    var posixClasses = {
      "[:alnum:]": ["\\p{L}\\p{Nl}\\p{Nd}", true],
      "[:alpha:]": ["\\p{L}\\p{Nl}", true],
      "[:ascii:]": ["\\x00-\\x7f", false],
      "[:blank:]": ["\\p{Zs}\\t", true],
      "[:cntrl:]": ["\\p{Cc}", true],
      "[:digit:]": ["\\p{Nd}", true],
      "[:graph:]": ["\\p{Z}\\p{C}", true, true],
      "[:lower:]": ["\\p{Ll}", true],
      "[:print:]": ["\\p{C}", true],
      "[:punct:]": ["\\p{P}", true],
      "[:space:]": ["\\p{Z}\\t\\r\\n\\v\\f", true],
      "[:upper:]": ["\\p{Lu}", true],
      "[:word:]": ["\\p{L}\\p{Nl}\\p{Nd}\\p{Pc}", true],
      "[:xdigit:]": ["A-Fa-f0-9", false]
    };
    var braceEscape = (s) => s.replace(/[[\]\\-]/g, "\\$&");
    var regexpEscape = (s) => s.replace(/[-[\]{}()*+?.,\\^$|#\s]/g, "\\$&");
    var rangesToString = (ranges) => ranges.join("");
    var parseClass = (glob, position) => {
      const pos = position;
      if (glob.charAt(pos) !== "[") {
        throw new Error("not in a brace expression");
      }
      const ranges = [];
      const negs = [];
      let i = pos + 1;
      let sawStart = false;
      let uflag = false;
      let escaping = false;
      let negate = false;
      let endPos = pos;
      let rangeStart = "";
      WHILE: while (i < glob.length) {
        const c = glob.charAt(i);
        if ((c === "!" || c === "^") && i === pos + 1) {
          negate = true;
          i++;
          continue;
        }
        if (c === "]" && sawStart && !escaping) {
          endPos = i + 1;
          break;
        }
        sawStart = true;
        if (c === "\\") {
          if (!escaping) {
            escaping = true;
            i++;
            continue;
          }
        }
        if (c === "[" && !escaping) {
          for (const [cls, [unip, u, neg]] of Object.entries(posixClasses)) {
            if (glob.startsWith(cls, i)) {
              if (rangeStart) {
                return ["$.", false, glob.length - pos, true];
              }
              i += cls.length;
              if (neg)
                negs.push(unip);
              else
                ranges.push(unip);
              uflag = uflag || u;
              continue WHILE;
            }
          }
        }
        escaping = false;
        if (rangeStart) {
          if (c > rangeStart) {
            ranges.push(braceEscape(rangeStart) + "-" + braceEscape(c));
          } else if (c === rangeStart) {
            ranges.push(braceEscape(c));
          }
          rangeStart = "";
          i++;
          continue;
        }
        if (glob.startsWith("-]", i + 1)) {
          ranges.push(braceEscape(c + "-"));
          i += 2;
          continue;
        }
        if (glob.startsWith("-", i + 1)) {
          rangeStart = c;
          i += 2;
          continue;
        }
        ranges.push(braceEscape(c));
        i++;
      }
      if (endPos < i) {
        return ["", false, 0, false];
      }
      if (!ranges.length && !negs.length) {
        return ["$.", false, glob.length - pos, true];
      }
      if (negs.length === 0 && ranges.length === 1 && /^\\?.$/.test(ranges[0]) && !negate) {
        const r = ranges[0].length === 2 ? ranges[0].slice(-1) : ranges[0];
        return [regexpEscape(r), false, endPos - pos, false];
      }
      const sranges = "[" + (negate ? "^" : "") + rangesToString(ranges) + "]";
      const snegs = "[" + (negate ? "" : "^") + rangesToString(negs) + "]";
      const comb = ranges.length && negs.length ? "(" + sranges + "|" + snegs + ")" : ranges.length ? sranges : snegs;
      return [comb, uflag, endPos - pos, true];
    };
    exports2.parseClass = parseClass;
  }
});

// node_modules/minimatch/dist/commonjs/unescape.js
var require_unescape = __commonJS({
  "node_modules/minimatch/dist/commonjs/unescape.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.unescape = void 0;
    var unescape = (s, { windowsPathsNoEscape = false, magicalBraces = true } = {}) => {
      if (magicalBraces) {
        return windowsPathsNoEscape ? s.replace(/\[([^/\\])\]/g, "$1") : s.replace(/((?!\\).|^)\[([^/\\])\]/g, "$1$2").replace(/\\([^/])/g, "$1");
      }
      return windowsPathsNoEscape ? s.replace(/\[([^/\\{}])\]/g, "$1") : s.replace(/((?!\\).|^)\[([^/\\{}])\]/g, "$1$2").replace(/\\([^/{}])/g, "$1");
    };
    exports2.unescape = unescape;
  }
});

// node_modules/minimatch/dist/commonjs/ast.js
var require_ast = __commonJS({
  "node_modules/minimatch/dist/commonjs/ast.js"(exports2) {
    "use strict";
    var _a;
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.AST = void 0;
    var brace_expressions_js_1 = require_brace_expressions();
    var unescape_js_1 = require_unescape();
    var types = /* @__PURE__ */ new Set(["!", "?", "+", "*", "@"]);
    var isExtglobType = (c) => types.has(c);
    var isExtglobAST = (c) => isExtglobType(c.type);
    var adoptionMap = /* @__PURE__ */ new Map([
      ["!", ["@"]],
      ["?", ["?", "@"]],
      ["@", ["@"]],
      ["*", ["*", "+", "?", "@"]],
      ["+", ["+", "@"]]
    ]);
    var adoptionWithSpaceMap = /* @__PURE__ */ new Map([
      ["!", ["?"]],
      ["@", ["?"]],
      ["+", ["?", "*"]]
    ]);
    var adoptionAnyMap = /* @__PURE__ */ new Map([
      ["!", ["?", "@"]],
      ["?", ["?", "@"]],
      ["@", ["?", "@"]],
      ["*", ["*", "+", "?", "@"]],
      ["+", ["+", "@", "?", "*"]]
    ]);
    var usurpMap = /* @__PURE__ */ new Map([
      ["!", /* @__PURE__ */ new Map([["!", "@"]])],
      [
        "?",
        /* @__PURE__ */ new Map([
          ["*", "*"],
          ["+", "*"]
        ])
      ],
      [
        "@",
        /* @__PURE__ */ new Map([
          ["!", "!"],
          ["?", "?"],
          ["@", "@"],
          ["*", "*"],
          ["+", "+"]
        ])
      ],
      [
        "+",
        /* @__PURE__ */ new Map([
          ["?", "*"],
          ["*", "*"]
        ])
      ]
    ]);
    var startNoTraversal = "(?!(?:^|/)\\.\\.?(?:$|/))";
    var startNoDot = "(?!\\.)";
    var addPatternStart = /* @__PURE__ */ new Set(["[", "."]);
    var justDots = /* @__PURE__ */ new Set(["..", "."]);
    var reSpecials = new Set("().*{}+?[]^$\\!");
    var regExpEscape = (s) => s.replace(/[-[\]{}()*+?.,\\^$|#\s]/g, "\\$&");
    var qmark = "[^/]";
    var star = qmark + "*?";
    var starNoEmpty = qmark + "+?";
    var ID = 0;
    var AST = class {
      type;
      #root;
      #hasMagic;
      #uflag = false;
      #parts = [];
      #parent;
      #parentIndex;
      #negs;
      #filledNegs = false;
      #options;
      #toString;
      // set to true if it's an extglob with no children
      // (which really means one child of '')
      #emptyExt = false;
      id = ++ID;
      get depth() {
        return (this.#parent?.depth ?? -1) + 1;
      }
      [/* @__PURE__ */ Symbol.for("nodejs.util.inspect.custom")]() {
        return {
          "@@type": "AST",
          id: this.id,
          type: this.type,
          root: this.#root.id,
          parent: this.#parent?.id,
          depth: this.depth,
          partsLength: this.#parts.length,
          parts: this.#parts
        };
      }
      constructor(type, parent, options = {}) {
        this.type = type;
        if (type)
          this.#hasMagic = true;
        this.#parent = parent;
        this.#root = this.#parent ? this.#parent.#root : this;
        this.#options = this.#root === this ? options : this.#root.#options;
        this.#negs = this.#root === this ? [] : this.#root.#negs;
        if (type === "!" && !this.#root.#filledNegs)
          this.#negs.push(this);
        this.#parentIndex = this.#parent ? this.#parent.#parts.length : 0;
      }
      get hasMagic() {
        if (this.#hasMagic !== void 0)
          return this.#hasMagic;
        for (const p of this.#parts) {
          if (typeof p === "string")
            continue;
          if (p.type || p.hasMagic)
            return this.#hasMagic = true;
        }
        return this.#hasMagic;
      }
      // reconstructs the pattern
      toString() {
        return this.#toString !== void 0 ? this.#toString : !this.type ? this.#toString = this.#parts.map((p) => String(p)).join("") : this.#toString = this.type + "(" + this.#parts.map((p) => String(p)).join("|") + ")";
      }
      #fillNegs() {
        if (this !== this.#root)
          throw new Error("should only call on root");
        if (this.#filledNegs)
          return this;
        this.toString();
        this.#filledNegs = true;
        let n;
        while (n = this.#negs.pop()) {
          if (n.type !== "!")
            continue;
          let p = n;
          let pp = p.#parent;
          while (pp) {
            for (let i = p.#parentIndex + 1; !pp.type && i < pp.#parts.length; i++) {
              for (const part of n.#parts) {
                if (typeof part === "string") {
                  throw new Error("string part in extglob AST??");
                }
                part.copyIn(pp.#parts[i]);
              }
            }
            p = pp;
            pp = p.#parent;
          }
        }
        return this;
      }
      push(...parts) {
        for (const p of parts) {
          if (p === "")
            continue;
          if (typeof p !== "string" && !(p instanceof _a && p.#parent === this)) {
            throw new Error("invalid part: " + p);
          }
          this.#parts.push(p);
        }
      }
      toJSON() {
        const ret = this.type === null ? this.#parts.slice().map((p) => typeof p === "string" ? p : p.toJSON()) : [this.type, ...this.#parts.map((p) => p.toJSON())];
        if (this.isStart() && !this.type)
          ret.unshift([]);
        if (this.isEnd() && (this === this.#root || this.#root.#filledNegs && this.#parent?.type === "!")) {
          ret.push({});
        }
        return ret;
      }
      isStart() {
        if (this.#root === this)
          return true;
        if (!this.#parent?.isStart())
          return false;
        if (this.#parentIndex === 0)
          return true;
        const p = this.#parent;
        for (let i = 0; i < this.#parentIndex; i++) {
          const pp = p.#parts[i];
          if (!(pp instanceof _a && pp.type === "!")) {
            return false;
          }
        }
        return true;
      }
      isEnd() {
        if (this.#root === this)
          return true;
        if (this.#parent?.type === "!")
          return true;
        if (!this.#parent?.isEnd())
          return false;
        if (!this.type)
          return this.#parent?.isEnd();
        const pl = this.#parent ? this.#parent.#parts.length : 0;
        return this.#parentIndex === pl - 1;
      }
      copyIn(part) {
        if (typeof part === "string")
          this.push(part);
        else
          this.push(part.clone(this));
      }
      clone(parent) {
        const c = new _a(this.type, parent);
        for (const p of this.#parts) {
          c.copyIn(p);
        }
        return c;
      }
      static #parseAST(str, ast, pos, opt, extDepth) {
        const maxDepth = opt.maxExtglobRecursion ?? 2;
        let escaping = false;
        let inBrace = false;
        let braceStart = -1;
        let braceNeg = false;
        if (ast.type === null) {
          let i2 = pos;
          let acc2 = "";
          while (i2 < str.length) {
            const c = str.charAt(i2++);
            if (escaping || c === "\\") {
              escaping = !escaping;
              acc2 += c;
              continue;
            }
            if (inBrace) {
              if (i2 === braceStart + 1) {
                if (c === "^" || c === "!") {
                  braceNeg = true;
                }
              } else if (c === "]" && !(i2 === braceStart + 2 && braceNeg)) {
                inBrace = false;
              }
              acc2 += c;
              continue;
            } else if (c === "[") {
              inBrace = true;
              braceStart = i2;
              braceNeg = false;
              acc2 += c;
              continue;
            }
            const doRecurse = !opt.noext && isExtglobType(c) && str.charAt(i2) === "(" && extDepth <= maxDepth;
            if (doRecurse) {
              ast.push(acc2);
              acc2 = "";
              const ext = new _a(c, ast);
              i2 = _a.#parseAST(str, ext, i2, opt, extDepth + 1);
              ast.push(ext);
              continue;
            }
            acc2 += c;
          }
          ast.push(acc2);
          return i2;
        }
        let i = pos + 1;
        let part = new _a(null, ast);
        const parts = [];
        let acc = "";
        while (i < str.length) {
          const c = str.charAt(i++);
          if (escaping || c === "\\") {
            escaping = !escaping;
            acc += c;
            continue;
          }
          if (inBrace) {
            if (i === braceStart + 1) {
              if (c === "^" || c === "!") {
                braceNeg = true;
              }
            } else if (c === "]" && !(i === braceStart + 2 && braceNeg)) {
              inBrace = false;
            }
            acc += c;
            continue;
          } else if (c === "[") {
            inBrace = true;
            braceStart = i;
            braceNeg = false;
            acc += c;
            continue;
          }
          const doRecurse = !opt.noext && isExtglobType(c) && str.charAt(i) === "(" && /* c8 ignore start - the maxDepth is sufficient here */
          (extDepth <= maxDepth || ast && ast.#canAdoptType(c));
          if (doRecurse) {
            const depthAdd = ast && ast.#canAdoptType(c) ? 0 : 1;
            part.push(acc);
            acc = "";
            const ext = new _a(c, part);
            part.push(ext);
            i = _a.#parseAST(str, ext, i, opt, extDepth + depthAdd);
            continue;
          }
          if (c === "|") {
            part.push(acc);
            acc = "";
            parts.push(part);
            part = new _a(null, ast);
            continue;
          }
          if (c === ")") {
            if (acc === "" && ast.#parts.length === 0) {
              ast.#emptyExt = true;
            }
            part.push(acc);
            acc = "";
            ast.push(...parts, part);
            return i;
          }
          acc += c;
        }
        ast.type = null;
        ast.#hasMagic = void 0;
        ast.#parts = [str.substring(pos - 1)];
        return i;
      }
      #canAdoptWithSpace(child) {
        return this.#canAdopt(child, adoptionWithSpaceMap);
      }
      #canAdopt(child, map = adoptionMap) {
        if (!child || typeof child !== "object" || child.type !== null || child.#parts.length !== 1 || this.type === null) {
          return false;
        }
        const gc = child.#parts[0];
        if (!gc || typeof gc !== "object" || gc.type === null) {
          return false;
        }
        return this.#canAdoptType(gc.type, map);
      }
      #canAdoptType(c, map = adoptionAnyMap) {
        return !!map.get(this.type)?.includes(c);
      }
      #adoptWithSpace(child, index) {
        const gc = child.#parts[0];
        const blank = new _a(null, gc, this.options);
        blank.#parts.push("");
        gc.push(blank);
        this.#adopt(child, index);
      }
      #adopt(child, index) {
        const gc = child.#parts[0];
        this.#parts.splice(index, 1, ...gc.#parts);
        for (const p of gc.#parts) {
          if (typeof p === "object")
            p.#parent = this;
        }
        this.#toString = void 0;
      }
      #canUsurpType(c) {
        const m = usurpMap.get(this.type);
        return !!m?.has(c);
      }
      #canUsurp(child) {
        if (!child || typeof child !== "object" || child.type !== null || child.#parts.length !== 1 || this.type === null || this.#parts.length !== 1) {
          return false;
        }
        const gc = child.#parts[0];
        if (!gc || typeof gc !== "object" || gc.type === null) {
          return false;
        }
        return this.#canUsurpType(gc.type);
      }
      #usurp(child) {
        const m = usurpMap.get(this.type);
        const gc = child.#parts[0];
        const nt = m?.get(gc.type);
        if (!nt)
          return false;
        this.#parts = gc.#parts;
        for (const p of this.#parts) {
          if (typeof p === "object") {
            p.#parent = this;
          }
        }
        this.type = nt;
        this.#toString = void 0;
        this.#emptyExt = false;
      }
      static fromGlob(pattern, options = {}) {
        const ast = new _a(null, void 0, options);
        _a.#parseAST(pattern, ast, 0, options, 0);
        return ast;
      }
      // returns the regular expression if there's magic, or the unescaped
      // string if not.
      toMMPattern() {
        if (this !== this.#root)
          return this.#root.toMMPattern();
        const glob = this.toString();
        const [re, body, hasMagic, uflag] = this.toRegExpSource();
        const anyMagic = hasMagic || this.#hasMagic || this.#options.nocase && !this.#options.nocaseMagicOnly && glob.toUpperCase() !== glob.toLowerCase();
        if (!anyMagic) {
          return body;
        }
        const flags = (this.#options.nocase ? "i" : "") + (uflag ? "u" : "");
        return Object.assign(new RegExp(`^${re}$`, flags), {
          _src: re,
          _glob: glob
        });
      }
      get options() {
        return this.#options;
      }
      // returns the string match, the regexp source, whether there's magic
      // in the regexp (so a regular expression is required) and whether or
      // not the uflag is needed for the regular expression (for posix classes)
      // TODO: instead of injecting the start/end at this point, just return
      // the BODY of the regexp, along with the start/end portions suitable
      // for binding the start/end in either a joined full-path makeRe context
      // (where we bind to (^|/), or a standalone matchPart context (where
      // we bind to ^, and not /).  Otherwise slashes get duped!
      //
      // In part-matching mode, the start is:
      // - if not isStart: nothing
      // - if traversal possible, but not allowed: ^(?!\.\.?$)
      // - if dots allowed or not possible: ^
      // - if dots possible and not allowed: ^(?!\.)
      // end is:
      // - if not isEnd(): nothing
      // - else: $
      //
      // In full-path matching mode, we put the slash at the START of the
      // pattern, so start is:
      // - if first pattern: same as part-matching mode
      // - if not isStart(): nothing
      // - if traversal possible, but not allowed: /(?!\.\.?(?:$|/))
      // - if dots allowed or not possible: /
      // - if dots possible and not allowed: /(?!\.)
      // end is:
      // - if last pattern, same as part-matching mode
      // - else nothing
      //
      // Always put the (?:$|/) on negated tails, though, because that has to be
      // there to bind the end of the negated pattern portion, and it's easier to
      // just stick it in now rather than try to inject it later in the middle of
      // the pattern.
      //
      // We can just always return the same end, and leave it up to the caller
      // to know whether it's going to be used joined or in parts.
      // And, if the start is adjusted slightly, can do the same there:
      // - if not isStart: nothing
      // - if traversal possible, but not allowed: (?:/|^)(?!\.\.?$)
      // - if dots allowed or not possible: (?:/|^)
      // - if dots possible and not allowed: (?:/|^)(?!\.)
      //
      // But it's better to have a simpler binding without a conditional, for
      // performance, so probably better to return both start options.
      //
      // Then the caller just ignores the end if it's not the first pattern,
      // and the start always gets applied.
      //
      // But that's always going to be $ if it's the ending pattern, or nothing,
      // so the caller can just attach $ at the end of the pattern when building.
      //
      // So the todo is:
      // - better detect what kind of start is needed
      // - return both flavors of starting pattern
      // - attach $ at the end of the pattern when creating the actual RegExp
      //
      // Ah, but wait, no, that all only applies to the root when the first pattern
      // is not an extglob. If the first pattern IS an extglob, then we need all
      // that dot prevention biz to live in the extglob portions, because eg
      // +(*|.x*) can match .xy but not .yx.
      //
      // So, return the two flavors if it's #root and the first child is not an
      // AST, otherwise leave it to the child AST to handle it, and there,
      // use the (?:^|/) style of start binding.
      //
      // Even simplified further:
      // - Since the start for a join is eg /(?!\.) and the start for a part
      // is ^(?!\.), we can just prepend (?!\.) to the pattern (either root
      // or start or whatever) and prepend ^ or / at the Regexp construction.
      toRegExpSource(allowDot) {
        const dot = allowDot ?? !!this.#options.dot;
        if (this.#root === this) {
          this.#flatten();
          this.#fillNegs();
        }
        if (!isExtglobAST(this)) {
          const noEmpty = this.isStart() && this.isEnd() && !this.#parts.some((s) => typeof s !== "string");
          const src = this.#parts.map((p) => {
            const [re, _, hasMagic, uflag] = typeof p === "string" ? _a.#parseGlob(p, this.#hasMagic, noEmpty) : p.toRegExpSource(allowDot);
            this.#hasMagic = this.#hasMagic || hasMagic;
            this.#uflag = this.#uflag || uflag;
            return re;
          }).join("");
          let start2 = "";
          if (this.isStart()) {
            if (typeof this.#parts[0] === "string") {
              const dotTravAllowed = this.#parts.length === 1 && justDots.has(this.#parts[0]);
              if (!dotTravAllowed) {
                const aps = addPatternStart;
                const needNoTrav = (
                  // dots are allowed, and the pattern starts with [ or .
                  dot && aps.has(src.charAt(0)) || // the pattern starts with \., and then [ or .
                  src.startsWith("\\.") && aps.has(src.charAt(2)) || // the pattern starts with \.\., and then [ or .
                  src.startsWith("\\.\\.") && aps.has(src.charAt(4))
                );
                const needNoDot = !dot && !allowDot && aps.has(src.charAt(0));
                start2 = needNoTrav ? startNoTraversal : needNoDot ? startNoDot : "";
              }
            }
          }
          let end = "";
          if (this.isEnd() && this.#root.#filledNegs && this.#parent?.type === "!") {
            end = "(?:$|\\/)";
          }
          const final2 = start2 + src + end;
          return [
            final2,
            (0, unescape_js_1.unescape)(src),
            this.#hasMagic = !!this.#hasMagic,
            this.#uflag
          ];
        }
        const repeated = this.type === "*" || this.type === "+";
        const start = this.type === "!" ? "(?:(?!(?:" : "(?:";
        let body = this.#partsToRegExp(dot);
        if (this.isStart() && this.isEnd() && !body && this.type !== "!") {
          const s = this.toString();
          const me = this;
          me.#parts = [s];
          me.type = null;
          me.#hasMagic = void 0;
          return [s, (0, unescape_js_1.unescape)(this.toString()), false, false];
        }
        let bodyDotAllowed = !repeated || allowDot || dot || !startNoDot ? "" : this.#partsToRegExp(true);
        if (bodyDotAllowed === body) {
          bodyDotAllowed = "";
        }
        if (bodyDotAllowed) {
          body = `(?:${body})(?:${bodyDotAllowed})*?`;
        }
        let final = "";
        if (this.type === "!" && this.#emptyExt) {
          final = (this.isStart() && !dot ? startNoDot : "") + starNoEmpty;
        } else {
          const close = this.type === "!" ? (
            // !() must match something,but !(x) can match ''
            "))" + (this.isStart() && !dot && !allowDot ? startNoDot : "") + star + ")"
          ) : this.type === "@" ? ")" : this.type === "?" ? ")?" : this.type === "+" && bodyDotAllowed ? ")" : this.type === "*" && bodyDotAllowed ? `)?` : `)${this.type}`;
          final = start + body + close;
        }
        return [
          final,
          (0, unescape_js_1.unescape)(body),
          this.#hasMagic = !!this.#hasMagic,
          this.#uflag
        ];
      }
      #flatten() {
        if (!isExtglobAST(this)) {
          for (const p of this.#parts) {
            if (typeof p === "object") {
              p.#flatten();
            }
          }
        } else {
          let iterations = 0;
          let done = false;
          do {
            done = true;
            for (let i = 0; i < this.#parts.length; i++) {
              const c = this.#parts[i];
              if (typeof c === "object") {
                c.#flatten();
                if (this.#canAdopt(c)) {
                  done = false;
                  this.#adopt(c, i);
                } else if (this.#canAdoptWithSpace(c)) {
                  done = false;
                  this.#adoptWithSpace(c, i);
                } else if (this.#canUsurp(c)) {
                  done = false;
                  this.#usurp(c);
                }
              }
            }
          } while (!done && ++iterations < 10);
        }
        this.#toString = void 0;
      }
      #partsToRegExp(dot) {
        return this.#parts.map((p) => {
          if (typeof p === "string") {
            throw new Error("string type in extglob ast??");
          }
          const [re, _, _hasMagic, uflag] = p.toRegExpSource(dot);
          this.#uflag = this.#uflag || uflag;
          return re;
        }).filter((p) => !(this.isStart() && this.isEnd()) || !!p).join("|");
      }
      static #parseGlob(glob, hasMagic, noEmpty = false) {
        let escaping = false;
        let re = "";
        let uflag = false;
        let inStar = false;
        for (let i = 0; i < glob.length; i++) {
          const c = glob.charAt(i);
          if (escaping) {
            escaping = false;
            re += (reSpecials.has(c) ? "\\" : "") + c;
            continue;
          }
          if (c === "*") {
            if (inStar)
              continue;
            inStar = true;
            re += noEmpty && /^[*]+$/.test(glob) ? starNoEmpty : star;
            hasMagic = true;
            continue;
          } else {
            inStar = false;
          }
          if (c === "\\") {
            if (i === glob.length - 1) {
              re += "\\\\";
            } else {
              escaping = true;
            }
            continue;
          }
          if (c === "[") {
            const [src, needUflag, consumed, magic] = (0, brace_expressions_js_1.parseClass)(glob, i);
            if (consumed) {
              re += src;
              uflag = uflag || needUflag;
              i += consumed - 1;
              hasMagic = hasMagic || magic;
              continue;
            }
          }
          if (c === "?") {
            re += qmark;
            hasMagic = true;
            continue;
          }
          re += regExpEscape(c);
        }
        return [re, (0, unescape_js_1.unescape)(glob), !!hasMagic, uflag];
      }
    };
    exports2.AST = AST;
    _a = AST;
  }
});

// node_modules/minimatch/dist/commonjs/escape.js
var require_escape = __commonJS({
  "node_modules/minimatch/dist/commonjs/escape.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.escape = void 0;
    var escape = (s, { windowsPathsNoEscape = false, magicalBraces = false } = {}) => {
      if (magicalBraces) {
        return windowsPathsNoEscape ? s.replace(/[?*()[\]{}]/g, "[$&]") : s.replace(/[?*()[\]\\{}]/g, "\\$&");
      }
      return windowsPathsNoEscape ? s.replace(/[?*()[\]]/g, "[$&]") : s.replace(/[?*()[\]\\]/g, "\\$&");
    };
    exports2.escape = escape;
  }
});

// node_modules/minimatch/dist/commonjs/index.js
var require_commonjs3 = __commonJS({
  "node_modules/minimatch/dist/commonjs/index.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.unescape = exports2.escape = exports2.AST = exports2.Minimatch = exports2.match = exports2.makeRe = exports2.braceExpand = exports2.defaults = exports2.filter = exports2.GLOBSTAR = exports2.sep = exports2.minimatch = void 0;
    var brace_expansion_1 = require_commonjs2();
    var assert_valid_pattern_js_1 = require_assert_valid_pattern();
    var ast_js_1 = require_ast();
    var escape_js_1 = require_escape();
    var unescape_js_1 = require_unescape();
    var minimatch = (p, pattern, options = {}) => {
      (0, assert_valid_pattern_js_1.assertValidPattern)(pattern);
      if (!options.nocomment && pattern.charAt(0) === "#") {
        return false;
      }
      return new Minimatch(pattern, options).match(p);
    };
    exports2.minimatch = minimatch;
    var starDotExtRE = /^\*+([^+@!?*[(]*)$/;
    var starDotExtTest = (ext2) => (f) => !f.startsWith(".") && f.endsWith(ext2);
    var starDotExtTestDot = (ext2) => (f) => f.endsWith(ext2);
    var starDotExtTestNocase = (ext2) => {
      ext2 = ext2.toLowerCase();
      return (f) => !f.startsWith(".") && f.toLowerCase().endsWith(ext2);
    };
    var starDotExtTestNocaseDot = (ext2) => {
      ext2 = ext2.toLowerCase();
      return (f) => f.toLowerCase().endsWith(ext2);
    };
    var starDotStarRE = /^\*+\.\*+$/;
    var starDotStarTest = (f) => !f.startsWith(".") && f.includes(".");
    var starDotStarTestDot = (f) => f !== "." && f !== ".." && f.includes(".");
    var dotStarRE = /^\.\*+$/;
    var dotStarTest = (f) => f !== "." && f !== ".." && f.startsWith(".");
    var starRE = /^\*+$/;
    var starTest = (f) => f.length !== 0 && !f.startsWith(".");
    var starTestDot = (f) => f.length !== 0 && f !== "." && f !== "..";
    var qmarksRE = /^\?+([^+@!?*[(]*)?$/;
    var qmarksTestNocase = ([$0, ext2 = ""]) => {
      const noext = qmarksTestNoExt([$0]);
      if (!ext2)
        return noext;
      ext2 = ext2.toLowerCase();
      return (f) => noext(f) && f.toLowerCase().endsWith(ext2);
    };
    var qmarksTestNocaseDot = ([$0, ext2 = ""]) => {
      const noext = qmarksTestNoExtDot([$0]);
      if (!ext2)
        return noext;
      ext2 = ext2.toLowerCase();
      return (f) => noext(f) && f.toLowerCase().endsWith(ext2);
    };
    var qmarksTestDot = ([$0, ext2 = ""]) => {
      const noext = qmarksTestNoExtDot([$0]);
      return !ext2 ? noext : (f) => noext(f) && f.endsWith(ext2);
    };
    var qmarksTest = ([$0, ext2 = ""]) => {
      const noext = qmarksTestNoExt([$0]);
      return !ext2 ? noext : (f) => noext(f) && f.endsWith(ext2);
    };
    var qmarksTestNoExt = ([$0]) => {
      const len = $0.length;
      return (f) => f.length === len && !f.startsWith(".");
    };
    var qmarksTestNoExtDot = ([$0]) => {
      const len = $0.length;
      return (f) => f.length === len && f !== "." && f !== "..";
    };
    var defaultPlatform = typeof process === "object" && process ? typeof process.env === "object" && process.env && process.env.__MINIMATCH_TESTING_PLATFORM__ || process.platform : "posix";
    var path = {
      win32: { sep: "\\" },
      posix: { sep: "/" }
    };
    exports2.sep = defaultPlatform === "win32" ? path.win32.sep : path.posix.sep;
    exports2.minimatch.sep = exports2.sep;
    exports2.GLOBSTAR = /* @__PURE__ */ Symbol("globstar **");
    exports2.minimatch.GLOBSTAR = exports2.GLOBSTAR;
    var qmark = "[^/]";
    var star = qmark + "*?";
    var twoStarDot = "(?:(?!(?:\\/|^)(?:\\.{1,2})($|\\/)).)*?";
    var twoStarNoDot = "(?:(?!(?:\\/|^)\\.).)*?";
    var filter = (pattern, options = {}) => (p) => (0, exports2.minimatch)(p, pattern, options);
    exports2.filter = filter;
    exports2.minimatch.filter = exports2.filter;
    var ext = (a, b = {}) => Object.assign({}, a, b);
    var defaults = (def) => {
      if (!def || typeof def !== "object" || !Object.keys(def).length) {
        return exports2.minimatch;
      }
      const orig = exports2.minimatch;
      const m = (p, pattern, options = {}) => orig(p, pattern, ext(def, options));
      return Object.assign(m, {
        Minimatch: class Minimatch extends orig.Minimatch {
          constructor(pattern, options = {}) {
            super(pattern, ext(def, options));
          }
          static defaults(options) {
            return orig.defaults(ext(def, options)).Minimatch;
          }
        },
        AST: class AST extends orig.AST {
          /* c8 ignore start */
          constructor(type, parent, options = {}) {
            super(type, parent, ext(def, options));
          }
          /* c8 ignore stop */
          static fromGlob(pattern, options = {}) {
            return orig.AST.fromGlob(pattern, ext(def, options));
          }
        },
        unescape: (s, options = {}) => orig.unescape(s, ext(def, options)),
        escape: (s, options = {}) => orig.escape(s, ext(def, options)),
        filter: (pattern, options = {}) => orig.filter(pattern, ext(def, options)),
        defaults: (options) => orig.defaults(ext(def, options)),
        makeRe: (pattern, options = {}) => orig.makeRe(pattern, ext(def, options)),
        braceExpand: (pattern, options = {}) => orig.braceExpand(pattern, ext(def, options)),
        match: (list, pattern, options = {}) => orig.match(list, pattern, ext(def, options)),
        sep: orig.sep,
        GLOBSTAR: exports2.GLOBSTAR
      });
    };
    exports2.defaults = defaults;
    exports2.minimatch.defaults = exports2.defaults;
    var braceExpand = (pattern, options = {}) => {
      (0, assert_valid_pattern_js_1.assertValidPattern)(pattern);
      if (options.nobrace || !/\{(?:(?!\{).)*\}/.test(pattern)) {
        return [pattern];
      }
      return (0, brace_expansion_1.expand)(pattern, { max: options.braceExpandMax });
    };
    exports2.braceExpand = braceExpand;
    exports2.minimatch.braceExpand = exports2.braceExpand;
    var makeRe = (pattern, options = {}) => new Minimatch(pattern, options).makeRe();
    exports2.makeRe = makeRe;
    exports2.minimatch.makeRe = exports2.makeRe;
    var match = (list, pattern, options = {}) => {
      const mm = new Minimatch(pattern, options);
      list = list.filter((f) => mm.match(f));
      if (mm.options.nonull && !list.length) {
        list.push(pattern);
      }
      return list;
    };
    exports2.match = match;
    exports2.minimatch.match = exports2.match;
    var globMagic = /[?*]|[+@!]\(.*?\)|\[|\]/;
    var regExpEscape = (s) => s.replace(/[-[\]{}()*+?.,\\^$|#\s]/g, "\\$&");
    var Minimatch = class {
      options;
      set;
      pattern;
      windowsPathsNoEscape;
      nonegate;
      negate;
      comment;
      empty;
      preserveMultipleSlashes;
      partial;
      globSet;
      globParts;
      nocase;
      isWindows;
      platform;
      windowsNoMagicRoot;
      maxGlobstarRecursion;
      regexp;
      constructor(pattern, options = {}) {
        (0, assert_valid_pattern_js_1.assertValidPattern)(pattern);
        options = options || {};
        this.options = options;
        this.maxGlobstarRecursion = options.maxGlobstarRecursion ?? 200;
        this.pattern = pattern;
        this.platform = options.platform || defaultPlatform;
        this.isWindows = this.platform === "win32";
        const awe = "allowWindowsEscape";
        this.windowsPathsNoEscape = !!options.windowsPathsNoEscape || options[awe] === false;
        if (this.windowsPathsNoEscape) {
          this.pattern = this.pattern.replace(/\\/g, "/");
        }
        this.preserveMultipleSlashes = !!options.preserveMultipleSlashes;
        this.regexp = null;
        this.negate = false;
        this.nonegate = !!options.nonegate;
        this.comment = false;
        this.empty = false;
        this.partial = !!options.partial;
        this.nocase = !!this.options.nocase;
        this.windowsNoMagicRoot = options.windowsNoMagicRoot !== void 0 ? options.windowsNoMagicRoot : !!(this.isWindows && this.nocase);
        this.globSet = [];
        this.globParts = [];
        this.set = [];
        this.make();
      }
      hasMagic() {
        if (this.options.magicalBraces && this.set.length > 1) {
          return true;
        }
        for (const pattern of this.set) {
          for (const part of pattern) {
            if (typeof part !== "string")
              return true;
          }
        }
        return false;
      }
      debug(..._) {
      }
      make() {
        const pattern = this.pattern;
        const options = this.options;
        if (!options.nocomment && pattern.charAt(0) === "#") {
          this.comment = true;
          return;
        }
        if (!pattern) {
          this.empty = true;
          return;
        }
        this.parseNegate();
        this.globSet = [...new Set(this.braceExpand())];
        if (options.debug) {
          this.debug = (...args) => console.error(...args);
        }
        this.debug(this.pattern, this.globSet);
        const rawGlobParts = this.globSet.map((s) => this.slashSplit(s));
        this.globParts = this.preprocess(rawGlobParts);
        this.debug(this.pattern, this.globParts);
        let set = this.globParts.map((s, _, __) => {
          if (this.isWindows && this.windowsNoMagicRoot) {
            const isUNC = s[0] === "" && s[1] === "" && (s[2] === "?" || !globMagic.test(s[2])) && !globMagic.test(s[3]);
            const isDrive = /^[a-z]:/i.test(s[0]);
            if (isUNC) {
              return [
                ...s.slice(0, 4),
                ...s.slice(4).map((ss) => this.parse(ss))
              ];
            } else if (isDrive) {
              return [s[0], ...s.slice(1).map((ss) => this.parse(ss))];
            }
          }
          return s.map((ss) => this.parse(ss));
        });
        this.debug(this.pattern, set);
        this.set = set.filter((s) => s.indexOf(false) === -1);
        if (this.isWindows) {
          for (let i = 0; i < this.set.length; i++) {
            const p = this.set[i];
            if (p[0] === "" && p[1] === "" && this.globParts[i][2] === "?" && typeof p[3] === "string" && /^[a-z]:$/i.test(p[3])) {
              p[2] = "?";
            }
          }
        }
        this.debug(this.pattern, this.set);
      }
      // various transforms to equivalent pattern sets that are
      // faster to process in a filesystem walk.  The goal is to
      // eliminate what we can, and push all ** patterns as far
      // to the right as possible, even if it increases the number
      // of patterns that we have to process.
      preprocess(globParts) {
        if (this.options.noglobstar) {
          for (const partset of globParts) {
            for (let j = 0; j < partset.length; j++) {
              if (partset[j] === "**") {
                partset[j] = "*";
              }
            }
          }
        }
        const { optimizationLevel = 1 } = this.options;
        if (optimizationLevel >= 2) {
          globParts = this.firstPhasePreProcess(globParts);
          globParts = this.secondPhasePreProcess(globParts);
        } else if (optimizationLevel >= 1) {
          globParts = this.levelOneOptimize(globParts);
        } else {
          globParts = this.adjascentGlobstarOptimize(globParts);
        }
        return globParts;
      }
      // just get rid of adjascent ** portions
      adjascentGlobstarOptimize(globParts) {
        return globParts.map((parts) => {
          let gs = -1;
          while (-1 !== (gs = parts.indexOf("**", gs + 1))) {
            let i = gs;
            while (parts[i + 1] === "**") {
              i++;
            }
            if (i !== gs) {
              parts.splice(gs, i - gs);
            }
          }
          return parts;
        });
      }
      // get rid of adjascent ** and resolve .. portions
      levelOneOptimize(globParts) {
        return globParts.map((parts) => {
          parts = parts.reduce((set, part) => {
            const prev = set[set.length - 1];
            if (part === "**" && prev === "**") {
              return set;
            }
            if (part === "..") {
              if (prev && prev !== ".." && prev !== "." && prev !== "**") {
                set.pop();
                return set;
              }
            }
            set.push(part);
            return set;
          }, []);
          return parts.length === 0 ? [""] : parts;
        });
      }
      levelTwoFileOptimize(parts) {
        if (!Array.isArray(parts)) {
          parts = this.slashSplit(parts);
        }
        let didSomething = false;
        do {
          didSomething = false;
          if (!this.preserveMultipleSlashes) {
            for (let i = 1; i < parts.length - 1; i++) {
              const p = parts[i];
              if (i === 1 && p === "" && parts[0] === "")
                continue;
              if (p === "." || p === "") {
                didSomething = true;
                parts.splice(i, 1);
                i--;
              }
            }
            if (parts[0] === "." && parts.length === 2 && (parts[1] === "." || parts[1] === "")) {
              didSomething = true;
              parts.pop();
            }
          }
          let dd = 0;
          while (-1 !== (dd = parts.indexOf("..", dd + 1))) {
            const p = parts[dd - 1];
            if (p && p !== "." && p !== ".." && p !== "**" && !(this.isWindows && /^[a-z]:$/i.test(p))) {
              didSomething = true;
              parts.splice(dd - 1, 2);
              dd -= 2;
            }
          }
        } while (didSomething);
        return parts.length === 0 ? [""] : parts;
      }
      // First phase: single-pattern processing
      // <pre> is 1 or more portions
      // <rest> is 1 or more portions
      // <p> is any portion other than ., .., '', or **
      // <e> is . or ''
      //
      // **/.. is *brutal* for filesystem walking performance, because
      // it effectively resets the recursive walk each time it occurs,
      // and ** cannot be reduced out by a .. pattern part like a regexp
      // or most strings (other than .., ., and '') can be.
      //
      // <pre>/**/../<p>/<p>/<rest> -> {<pre>/../<p>/<p>/<rest>,<pre>/**/<p>/<p>/<rest>}
      // <pre>/<e>/<rest> -> <pre>/<rest>
      // <pre>/<p>/../<rest> -> <pre>/<rest>
      // **/**/<rest> -> **/<rest>
      //
      // **/*/<rest> -> */**/<rest> <== not valid because ** doesn't follow
      // this WOULD be allowed if ** did follow symlinks, or * didn't
      firstPhasePreProcess(globParts) {
        let didSomething = false;
        do {
          didSomething = false;
          for (let parts of globParts) {
            let gs = -1;
            while (-1 !== (gs = parts.indexOf("**", gs + 1))) {
              let gss = gs;
              while (parts[gss + 1] === "**") {
                gss++;
              }
              if (gss > gs) {
                parts.splice(gs + 1, gss - gs);
              }
              let next = parts[gs + 1];
              const p = parts[gs + 2];
              const p2 = parts[gs + 3];
              if (next !== "..")
                continue;
              if (!p || p === "." || p === ".." || !p2 || p2 === "." || p2 === "..") {
                continue;
              }
              didSomething = true;
              parts.splice(gs, 1);
              const other = parts.slice(0);
              other[gs] = "**";
              globParts.push(other);
              gs--;
            }
            if (!this.preserveMultipleSlashes) {
              for (let i = 1; i < parts.length - 1; i++) {
                const p = parts[i];
                if (i === 1 && p === "" && parts[0] === "")
                  continue;
                if (p === "." || p === "") {
                  didSomething = true;
                  parts.splice(i, 1);
                  i--;
                }
              }
              if (parts[0] === "." && parts.length === 2 && (parts[1] === "." || parts[1] === "")) {
                didSomething = true;
                parts.pop();
              }
            }
            let dd = 0;
            while (-1 !== (dd = parts.indexOf("..", dd + 1))) {
              const p = parts[dd - 1];
              if (p && p !== "." && p !== ".." && p !== "**") {
                didSomething = true;
                const needDot = dd === 1 && parts[dd + 1] === "**";
                const splin = needDot ? ["."] : [];
                parts.splice(dd - 1, 2, ...splin);
                if (parts.length === 0)
                  parts.push("");
                dd -= 2;
              }
            }
          }
        } while (didSomething);
        return globParts;
      }
      // second phase: multi-pattern dedupes
      // {<pre>/*/<rest>,<pre>/<p>/<rest>} -> <pre>/*/<rest>
      // {<pre>/<rest>,<pre>/<rest>} -> <pre>/<rest>
      // {<pre>/**/<rest>,<pre>/<rest>} -> <pre>/**/<rest>
      //
      // {<pre>/**/<rest>,<pre>/**/<p>/<rest>} -> <pre>/**/<rest>
      // ^-- not valid because ** doens't follow symlinks
      secondPhasePreProcess(globParts) {
        for (let i = 0; i < globParts.length - 1; i++) {
          for (let j = i + 1; j < globParts.length; j++) {
            const matched = this.partsMatch(globParts[i], globParts[j], !this.preserveMultipleSlashes);
            if (matched) {
              globParts[i] = [];
              globParts[j] = matched;
              break;
            }
          }
        }
        return globParts.filter((gs) => gs.length);
      }
      partsMatch(a, b, emptyGSMatch = false) {
        let ai = 0;
        let bi = 0;
        let result = [];
        let which = "";
        while (ai < a.length && bi < b.length) {
          if (a[ai] === b[bi]) {
            result.push(which === "b" ? b[bi] : a[ai]);
            ai++;
            bi++;
          } else if (emptyGSMatch && a[ai] === "**" && b[bi] === a[ai + 1]) {
            result.push(a[ai]);
            ai++;
          } else if (emptyGSMatch && b[bi] === "**" && a[ai] === b[bi + 1]) {
            result.push(b[bi]);
            bi++;
          } else if (a[ai] === "*" && b[bi] && (this.options.dot || !b[bi].startsWith(".")) && b[bi] !== "**") {
            if (which === "b")
              return false;
            which = "a";
            result.push(a[ai]);
            ai++;
            bi++;
          } else if (b[bi] === "*" && a[ai] && (this.options.dot || !a[ai].startsWith(".")) && a[ai] !== "**") {
            if (which === "a")
              return false;
            which = "b";
            result.push(b[bi]);
            ai++;
            bi++;
          } else {
            return false;
          }
        }
        return a.length === b.length && result;
      }
      parseNegate() {
        if (this.nonegate)
          return;
        const pattern = this.pattern;
        let negate = false;
        let negateOffset = 0;
        for (let i = 0; i < pattern.length && pattern.charAt(i) === "!"; i++) {
          negate = !negate;
          negateOffset++;
        }
        if (negateOffset)
          this.pattern = pattern.slice(negateOffset);
        this.negate = negate;
      }
      // set partial to true to test if, for example,
      // "/a/b" matches the start of "/*/b/*/d"
      // Partial means, if you run out of file before you run
      // out of pattern, then that's fine, as long as all
      // the parts match.
      matchOne(file, pattern, partial = false) {
        let fileStartIndex = 0;
        let patternStartIndex = 0;
        if (this.isWindows) {
          const fileDrive = typeof file[0] === "string" && /^[a-z]:$/i.test(file[0]);
          const fileUNC = !fileDrive && file[0] === "" && file[1] === "" && file[2] === "?" && /^[a-z]:$/i.test(file[3]);
          const patternDrive = typeof pattern[0] === "string" && /^[a-z]:$/i.test(pattern[0]);
          const patternUNC = !patternDrive && pattern[0] === "" && pattern[1] === "" && pattern[2] === "?" && typeof pattern[3] === "string" && /^[a-z]:$/i.test(pattern[3]);
          const fdi = fileUNC ? 3 : fileDrive ? 0 : void 0;
          const pdi = patternUNC ? 3 : patternDrive ? 0 : void 0;
          if (typeof fdi === "number" && typeof pdi === "number") {
            const [fd, pd] = [
              file[fdi],
              pattern[pdi]
            ];
            if (fd.toLowerCase() === pd.toLowerCase()) {
              pattern[pdi] = fd;
              patternStartIndex = pdi;
              fileStartIndex = fdi;
            }
          }
        }
        const { optimizationLevel = 1 } = this.options;
        if (optimizationLevel >= 2) {
          file = this.levelTwoFileOptimize(file);
        }
        if (pattern.includes(exports2.GLOBSTAR)) {
          return this.#matchGlobstar(file, pattern, partial, fileStartIndex, patternStartIndex);
        }
        return this.#matchOne(file, pattern, partial, fileStartIndex, patternStartIndex);
      }
      #matchGlobstar(file, pattern, partial, fileIndex, patternIndex) {
        const firstgs = pattern.indexOf(exports2.GLOBSTAR, patternIndex);
        const lastgs = pattern.lastIndexOf(exports2.GLOBSTAR);
        const [head, body, tail] = partial ? [
          pattern.slice(patternIndex, firstgs),
          pattern.slice(firstgs + 1),
          []
        ] : [
          pattern.slice(patternIndex, firstgs),
          pattern.slice(firstgs + 1, lastgs),
          pattern.slice(lastgs + 1)
        ];
        if (head.length) {
          const fileHead = file.slice(fileIndex, fileIndex + head.length);
          if (!this.#matchOne(fileHead, head, partial, 0, 0)) {
            return false;
          }
          fileIndex += head.length;
          patternIndex += head.length;
        }
        let fileTailMatch = 0;
        if (tail.length) {
          if (tail.length + fileIndex > file.length)
            return false;
          let tailStart = file.length - tail.length;
          if (this.#matchOne(file, tail, partial, tailStart, 0)) {
            fileTailMatch = tail.length;
          } else {
            if (file[file.length - 1] !== "" || fileIndex + tail.length === file.length) {
              return false;
            }
            tailStart--;
            if (!this.#matchOne(file, tail, partial, tailStart, 0)) {
              return false;
            }
            fileTailMatch = tail.length + 1;
          }
        }
        if (!body.length) {
          let sawSome = !!fileTailMatch;
          for (let i2 = fileIndex; i2 < file.length - fileTailMatch; i2++) {
            const f = String(file[i2]);
            sawSome = true;
            if (f === "." || f === ".." || !this.options.dot && f.startsWith(".")) {
              return false;
            }
          }
          return partial || sawSome;
        }
        const bodySegments = [[[], 0]];
        let currentBody = bodySegments[0];
        let nonGsParts = 0;
        const nonGsPartsSums = [0];
        for (const b of body) {
          if (b === exports2.GLOBSTAR) {
            nonGsPartsSums.push(nonGsParts);
            currentBody = [[], 0];
            bodySegments.push(currentBody);
          } else {
            currentBody[0].push(b);
            nonGsParts++;
          }
        }
        let i = bodySegments.length - 1;
        const fileLength = file.length - fileTailMatch;
        for (const b of bodySegments) {
          b[1] = fileLength - (nonGsPartsSums[i--] + b[0].length);
        }
        return !!this.#matchGlobStarBodySections(file, bodySegments, fileIndex, 0, partial, 0, !!fileTailMatch);
      }
      // return false for "nope, not matching"
      // return null for "not matching, cannot keep trying"
      #matchGlobStarBodySections(file, bodySegments, fileIndex, bodyIndex, partial, globStarDepth, sawTail) {
        const bs = bodySegments[bodyIndex];
        if (!bs) {
          for (let i = fileIndex; i < file.length; i++) {
            sawTail = true;
            const f = file[i];
            if (f === "." || f === ".." || !this.options.dot && f.startsWith(".")) {
              return false;
            }
          }
          return sawTail;
        }
        const [body, after] = bs;
        while (fileIndex <= after) {
          const m = this.#matchOne(file.slice(0, fileIndex + body.length), body, partial, fileIndex, 0);
          if (m && globStarDepth < this.maxGlobstarRecursion) {
            const sub = this.#matchGlobStarBodySections(file, bodySegments, fileIndex + body.length, bodyIndex + 1, partial, globStarDepth + 1, sawTail);
            if (sub !== false) {
              return sub;
            }
          }
          const f = file[fileIndex];
          if (f === "." || f === ".." || !this.options.dot && f.startsWith(".")) {
            return false;
          }
          fileIndex++;
        }
        return partial || null;
      }
      #matchOne(file, pattern, partial, fileIndex, patternIndex) {
        let fi;
        let pi;
        let pl;
        let fl;
        for (fi = fileIndex, pi = patternIndex, fl = file.length, pl = pattern.length; fi < fl && pi < pl; fi++, pi++) {
          this.debug("matchOne loop");
          let p = pattern[pi];
          let f = file[fi];
          this.debug(pattern, p, f);
          if (p === false || p === exports2.GLOBSTAR) {
            return false;
          }
          let hit;
          if (typeof p === "string") {
            hit = f === p;
            this.debug("string match", p, f, hit);
          } else {
            hit = p.test(f);
            this.debug("pattern match", p, f, hit);
          }
          if (!hit)
            return false;
        }
        if (fi === fl && pi === pl) {
          return true;
        } else if (fi === fl) {
          return partial;
        } else if (pi === pl) {
          return fi === fl - 1 && file[fi] === "";
        } else {
          throw new Error("wtf?");
        }
      }
      braceExpand() {
        return (0, exports2.braceExpand)(this.pattern, this.options);
      }
      parse(pattern) {
        (0, assert_valid_pattern_js_1.assertValidPattern)(pattern);
        const options = this.options;
        if (pattern === "**")
          return exports2.GLOBSTAR;
        if (pattern === "")
          return "";
        let m;
        let fastTest = null;
        if (m = pattern.match(starRE)) {
          fastTest = options.dot ? starTestDot : starTest;
        } else if (m = pattern.match(starDotExtRE)) {
          fastTest = (options.nocase ? options.dot ? starDotExtTestNocaseDot : starDotExtTestNocase : options.dot ? starDotExtTestDot : starDotExtTest)(m[1]);
        } else if (m = pattern.match(qmarksRE)) {
          fastTest = (options.nocase ? options.dot ? qmarksTestNocaseDot : qmarksTestNocase : options.dot ? qmarksTestDot : qmarksTest)(m);
        } else if (m = pattern.match(starDotStarRE)) {
          fastTest = options.dot ? starDotStarTestDot : starDotStarTest;
        } else if (m = pattern.match(dotStarRE)) {
          fastTest = dotStarTest;
        }
        const re = ast_js_1.AST.fromGlob(pattern, this.options).toMMPattern();
        if (fastTest && typeof re === "object") {
          Reflect.defineProperty(re, "test", { value: fastTest });
        }
        return re;
      }
      makeRe() {
        if (this.regexp || this.regexp === false)
          return this.regexp;
        const set = this.set;
        if (!set.length) {
          this.regexp = false;
          return this.regexp;
        }
        const options = this.options;
        const twoStar = options.noglobstar ? star : options.dot ? twoStarDot : twoStarNoDot;
        const flags = new Set(options.nocase ? ["i"] : []);
        let re = set.map((pattern) => {
          const pp = pattern.map((p) => {
            if (p instanceof RegExp) {
              for (const f of p.flags.split(""))
                flags.add(f);
            }
            return typeof p === "string" ? regExpEscape(p) : p === exports2.GLOBSTAR ? exports2.GLOBSTAR : p._src;
          });
          pp.forEach((p, i) => {
            const next = pp[i + 1];
            const prev = pp[i - 1];
            if (p !== exports2.GLOBSTAR || prev === exports2.GLOBSTAR) {
              return;
            }
            if (prev === void 0) {
              if (next !== void 0 && next !== exports2.GLOBSTAR) {
                pp[i + 1] = "(?:\\/|" + twoStar + "\\/)?" + next;
              } else {
                pp[i] = twoStar;
              }
            } else if (next === void 0) {
              pp[i - 1] = prev + "(?:\\/|\\/" + twoStar + ")?";
            } else if (next !== exports2.GLOBSTAR) {
              pp[i - 1] = prev + "(?:\\/|\\/" + twoStar + "\\/)" + next;
              pp[i + 1] = exports2.GLOBSTAR;
            }
          });
          const filtered = pp.filter((p) => p !== exports2.GLOBSTAR);
          if (this.partial && filtered.length >= 1) {
            const prefixes = [];
            for (let i = 1; i <= filtered.length; i++) {
              prefixes.push(filtered.slice(0, i).join("/"));
            }
            return "(?:" + prefixes.join("|") + ")";
          }
          return filtered.join("/");
        }).join("|");
        const [open, close] = set.length > 1 ? ["(?:", ")"] : ["", ""];
        re = "^" + open + re + close + "$";
        if (this.partial) {
          re = "^(?:\\/|" + open + re.slice(1, -1) + close + ")$";
        }
        if (this.negate)
          re = "^(?!" + re + ").+$";
        try {
          this.regexp = new RegExp(re, [...flags].join(""));
        } catch {
          this.regexp = false;
        }
        return this.regexp;
      }
      slashSplit(p) {
        if (this.preserveMultipleSlashes) {
          return p.split("/");
        } else if (this.isWindows && /^\/\/[^/]+/.test(p)) {
          return ["", ...p.split(/\/+/)];
        } else {
          return p.split(/\/+/);
        }
      }
      match(f, partial = this.partial) {
        this.debug("match", f, this.pattern);
        if (this.comment) {
          return false;
        }
        if (this.empty) {
          return f === "";
        }
        if (f === "/" && partial) {
          return true;
        }
        const options = this.options;
        if (this.isWindows) {
          f = f.split("\\").join("/");
        }
        const ff = this.slashSplit(f);
        this.debug(this.pattern, "split", ff);
        const set = this.set;
        this.debug(this.pattern, "set", set);
        let filename = ff[ff.length - 1];
        if (!filename) {
          for (let i = ff.length - 2; !filename && i >= 0; i--) {
            filename = ff[i];
          }
        }
        for (const pattern of set) {
          let file = ff;
          if (options.matchBase && pattern.length === 1) {
            file = [filename];
          }
          const hit = this.matchOne(file, pattern, partial);
          if (hit) {
            if (options.flipNegate) {
              return true;
            }
            return !this.negate;
          }
        }
        if (options.flipNegate) {
          return false;
        }
        return this.negate;
      }
      static defaults(def) {
        return exports2.minimatch.defaults(def).Minimatch;
      }
    };
    exports2.Minimatch = Minimatch;
    var ast_js_2 = require_ast();
    Object.defineProperty(exports2, "AST", { enumerable: true, get: function() {
      return ast_js_2.AST;
    } });
    var escape_js_2 = require_escape();
    Object.defineProperty(exports2, "escape", { enumerable: true, get: function() {
      return escape_js_2.escape;
    } });
    var unescape_js_2 = require_unescape();
    Object.defineProperty(exports2, "unescape", { enumerable: true, get: function() {
      return unescape_js_2.unescape;
    } });
    exports2.minimatch.AST = ast_js_1.AST;
    exports2.minimatch.Minimatch = Minimatch;
    exports2.minimatch.escape = escape_js_1.escape;
    exports2.minimatch.unescape = unescape_js_1.unescape;
  }
});

// node_modules/vscode-languageclient/lib/common/utils/globPattern.js
var require_globPattern = __commonJS({
  "node_modules/vscode-languageclient/lib/common/utils/globPattern.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.matchGlobPattern = matchGlobPattern;
    var minimatch = __importStar(require_commonjs3());
    var vscode_1 = require("vscode");
    function matchGlobPattern(pattern, resource) {
      let miniMatchPattern;
      if (typeof pattern === "string") {
        miniMatchPattern = pattern.replace(/\\/g, "/");
      } else {
        try {
          const baseUri = vscode_1.Uri.parse(typeof pattern.baseUri === "string" ? pattern.baseUri : pattern.baseUri.uri);
          miniMatchPattern = baseUri.with({ path: baseUri.path + "/" + pattern.pattern }).fsPath.replace(/\\/g, "/");
        } catch (error) {
          return false;
        }
      }
      const matcher = new minimatch.Minimatch(miniMatchPattern, { noext: true });
      if (!matcher.makeRe()) {
        return false;
      }
      return matcher.match(resource.fsPath);
    }
  }
});

// node_modules/vscode-languageclient/lib/common/diagnostic.js
var require_diagnostic = __commonJS({
  "node_modules/vscode-languageclient/lib/common/diagnostic.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DiagnosticFeature = exports2.DiagnosticPullMode = exports2.vsdiag = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var uuid_1 = require_uuid();
    var globPattern_1 = require_globPattern();
    var features_1 = require_features();
    function ensure(target, key) {
      if (target[key] === void 0) {
        target[key] = {};
      }
      return target[key];
    }
    var vsdiag;
    (function(vsdiag2) {
      let DocumentDiagnosticReportKind;
      (function(DocumentDiagnosticReportKind2) {
        DocumentDiagnosticReportKind2["full"] = "full";
        DocumentDiagnosticReportKind2["unChanged"] = "unChanged";
      })(DocumentDiagnosticReportKind = vsdiag2.DocumentDiagnosticReportKind || (vsdiag2.DocumentDiagnosticReportKind = {}));
    })(vsdiag || (exports2.vsdiag = vsdiag = {}));
    var DiagnosticPullMode;
    (function(DiagnosticPullMode2) {
      DiagnosticPullMode2["onType"] = "onType";
      DiagnosticPullMode2["onSave"] = "onSave";
      DiagnosticPullMode2["onFocus"] = "onFocus";
    })(DiagnosticPullMode || (exports2.DiagnosticPullMode = DiagnosticPullMode = {}));
    var RequestStateKind;
    (function(RequestStateKind2) {
      RequestStateKind2["active"] = "open";
      RequestStateKind2["reschedule"] = "reschedule";
      RequestStateKind2["outDated"] = "drop";
    })(RequestStateKind || (RequestStateKind = {}));
    var PullState;
    (function(PullState2) {
      PullState2[PullState2["document"] = 1] = "document";
      PullState2[PullState2["workspace"] = 2] = "workspace";
    })(PullState || (PullState = {}));
    var DocumentOrUri;
    (function(DocumentOrUri2) {
      function asKey(document) {
        return document instanceof vscode_1.Uri ? document.toString() : document.uri.toString();
      }
      DocumentOrUri2.asKey = asKey;
    })(DocumentOrUri || (DocumentOrUri = {}));
    var DocumentPullStateTracker = class {
      documentPullStates;
      workspacePullStates;
      constructor() {
        this.documentPullStates = /* @__PURE__ */ new Map();
        this.workspacePullStates = /* @__PURE__ */ new Map();
      }
      track(kind, document, arg1) {
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        const [key, uri, version] = document instanceof vscode_1.Uri ? [document.toString(), document, arg1] : [document.uri.toString(), document.uri, document.version];
        let state = states.get(key);
        if (state === void 0) {
          state = { document: uri, pulledVersion: version, resultId: void 0 };
          states.set(key, state);
        }
        return state;
      }
      update(kind, document, arg1, arg2) {
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        const [key, uri, version, resultId] = document instanceof vscode_1.Uri ? [document.toString(), document, arg1, arg2] : [document.uri.toString(), document.uri, document.version, arg1];
        let state = states.get(key);
        if (state === void 0) {
          state = { document: uri, pulledVersion: version, resultId };
          states.set(key, state);
        } else {
          state.pulledVersion = version;
          state.resultId = resultId;
        }
      }
      unTrack(kind, document) {
        const key = DocumentOrUri.asKey(document);
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        states.delete(key);
      }
      tracks(kind, document) {
        const key = DocumentOrUri.asKey(document);
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        return states.has(key);
      }
      tracksSameVersion(kind, document) {
        const key = document.uri.toString();
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        const state = states.get(key);
        return state !== void 0 && state.pulledVersion === document.version;
      }
      getResultId(kind, document) {
        const key = DocumentOrUri.asKey(document);
        const states = kind === PullState.document ? this.documentPullStates : this.workspacePullStates;
        return states.get(key)?.resultId;
      }
      getAllResultIds() {
        const result = [];
        for (let [uri, value] of this.workspacePullStates) {
          if (this.documentPullStates.has(uri)) {
            value = this.documentPullStates.get(uri);
          }
          if (value.resultId !== void 0) {
            result.push({ uri, value: value.resultId });
          }
        }
        return result;
      }
    };
    var DiagnosticRequestor = class {
      isDisposed;
      client;
      visibleDocuments;
      options;
      onDidChangeDiagnosticsEmitter;
      provider;
      diagnostics;
      openRequests;
      documentStates;
      workspaceErrorCounter;
      workspaceCancellation;
      workspaceTimeout;
      constructor(client, visibleDocuments, options) {
        this.client = client;
        this.visibleDocuments = visibleDocuments;
        this.options = options;
        this.isDisposed = false;
        this.onDidChangeDiagnosticsEmitter = new vscode_1.EventEmitter();
        this.provider = this.createProvider();
        this.diagnostics = this.createDiagnosticCollection();
        this.openRequests = /* @__PURE__ */ new Map();
        this.documentStates = new DocumentPullStateTracker();
        this.workspaceErrorCounter = 0;
      }
      createDiagnosticCollection() {
        if (this.client.clientOptions.diagnosticCollectionProvider === void 0) {
          return vscode_1.languages.createDiagnosticCollection(this.options.identifier);
        } else {
          return this.client.clientOptions.diagnosticCollectionProvider.create(this.options.identifier, features_1.DiagnosticCollectionSource.pull);
        }
      }
      knows(kind, document) {
        const uri = document instanceof vscode_1.Uri ? document : document.uri;
        return this.documentStates.tracks(kind, document) || this.openRequests.has(uri.toString());
      }
      knowsSameVersion(kind, document) {
        const requestState = this.openRequests.get(document.uri.toString());
        if (requestState === void 0) {
          return this.documentStates.tracksSameVersion(kind, document);
        }
        if (requestState.state === RequestStateKind.reschedule) {
          return true;
        }
        if (requestState.state === RequestStateKind.outDated) {
          return false;
        }
        return requestState.version === document.version;
      }
      forget(kind, document) {
        this.documentStates.unTrack(kind, document);
      }
      pull(document, cb) {
        if (this.isDisposed) {
          return;
        }
        const uri = document instanceof vscode_1.Uri ? document : document.uri;
        this.pullAsync(document).then(() => {
          if (cb) {
            cb();
          }
        }, (error) => {
          this.client.error(`Document pull failed for text document ${uri.toString()}`, error, false);
        });
      }
      async pullAsync(document, version) {
        if (this.isDisposed) {
          return;
        }
        const isUri = document instanceof vscode_1.Uri;
        const uri = isUri ? document : document.uri;
        const key = uri.toString();
        version = isUri ? version : document.version;
        const currentRequestState = this.openRequests.get(key);
        const documentState = isUri ? this.documentStates.track(PullState.document, document, version) : this.documentStates.track(PullState.document, document);
        if (currentRequestState === void 0) {
          const tokenSource = new vscode_1.CancellationTokenSource();
          this.openRequests.set(key, { state: RequestStateKind.active, document, version, tokenSource });
          let report;
          let afterState;
          try {
            report = await this.provider.provideDiagnostics(document, documentState.resultId, tokenSource.token) ?? { kind: vsdiag.DocumentDiagnosticReportKind.full, items: [] };
          } catch (error) {
            if (error instanceof features_1.LSPCancellationError && vscode_languageserver_protocol_1.DiagnosticServerCancellationData.is(error.data) && error.data.retriggerRequest === false) {
              afterState = { state: RequestStateKind.outDated, document };
            }
            if (afterState === void 0 && error instanceof vscode_1.CancellationError) {
              afterState = { state: RequestStateKind.reschedule, document };
            } else {
              throw error;
            }
          }
          afterState = afterState ?? this.openRequests.get(key);
          if (afterState === void 0) {
            this.client.error(`Lost request state in diagnostic pull model. Clearing diagnostics for ${key}`);
            this.diagnostics.delete(uri);
            return;
          }
          this.openRequests.delete(key);
          if (!this.visibleDocuments.isVisible(document)) {
            this.documentStates.unTrack(PullState.document, document);
            return;
          }
          if (afterState.state === RequestStateKind.outDated) {
            return;
          }
          if (report !== void 0) {
            if (report.kind === vsdiag.DocumentDiagnosticReportKind.full) {
              this.diagnostics.set(uri, report.items);
            }
            documentState.pulledVersion = version;
            documentState.resultId = report.resultId;
          }
          if (afterState.state === RequestStateKind.reschedule) {
            this.pull(document);
          }
        } else {
          if (currentRequestState.state === RequestStateKind.active) {
            currentRequestState.tokenSource.cancel();
            this.openRequests.set(key, { state: RequestStateKind.reschedule, document: currentRequestState.document });
          } else if (currentRequestState.state === RequestStateKind.outDated) {
            this.openRequests.set(key, { state: RequestStateKind.reschedule, document: currentRequestState.document });
          }
        }
      }
      forgetDocument(document) {
        if (this.isDisposed) {
          return;
        }
        const uri = document instanceof vscode_1.Uri ? document : document.uri;
        const key = uri.toString();
        const request = this.openRequests.get(key);
        if (this.options.workspaceDiagnostics && uri.scheme !== "untitled") {
          if (request !== void 0) {
            this.openRequests.set(key, { state: RequestStateKind.reschedule, document });
          } else {
            this.pull(document, () => {
              this.forget(PullState.document, document);
            });
          }
          this.forget(PullState.workspace, document);
        } else {
          if (request !== void 0) {
            if (request.state === RequestStateKind.active) {
              request.tokenSource.cancel();
            }
            this.openRequests.set(key, { state: RequestStateKind.outDated, document });
          }
          this.diagnostics.delete(uri);
          this.forget(PullState.document, document);
        }
      }
      pullWorkspace() {
        if (this.isDisposed) {
          return;
        }
        this.pullWorkspaceAsync().then(() => {
          this.workspaceTimeout = (0, vscode_languageserver_protocol_1.RAL)().timer.setTimeout(() => {
            this.pullWorkspace();
          }, 2e3);
        }, (error) => {
          if (!(error instanceof features_1.LSPCancellationError) && !vscode_languageserver_protocol_1.DiagnosticServerCancellationData.is(error.data)) {
            this.client.error(`Workspace diagnostic pull failed.`, error, false);
            this.workspaceErrorCounter++;
          }
          if (this.workspaceErrorCounter <= 5) {
            this.workspaceTimeout = (0, vscode_languageserver_protocol_1.RAL)().timer.setTimeout(() => {
              this.pullWorkspace();
            }, 2e3);
          }
        });
      }
      async pullWorkspaceAsync() {
        if (!this.provider.provideWorkspaceDiagnostics || this.isDisposed) {
          return;
        }
        if (this.workspaceCancellation !== void 0) {
          this.workspaceCancellation.cancel();
          this.workspaceCancellation = void 0;
        }
        this.workspaceCancellation = new vscode_1.CancellationTokenSource();
        const previousResultIds = this.documentStates.getAllResultIds().map((item) => {
          return {
            uri: this.client.protocol2CodeConverter.asUri(item.uri),
            value: item.value
          };
        });
        await this.provider.provideWorkspaceDiagnostics(previousResultIds, this.workspaceCancellation.token, (chunk) => {
          if (!chunk || this.isDisposed) {
            return;
          }
          for (const item of chunk.items) {
            if (item.kind === vsdiag.DocumentDiagnosticReportKind.full) {
              if (!this.documentStates.tracks(PullState.document, item.uri)) {
                this.diagnostics.set(item.uri, item.items);
              }
            }
            this.documentStates.update(PullState.workspace, item.uri, item.version ?? void 0, item.resultId);
          }
        });
      }
      createProvider() {
        const result = {
          onDidChangeDiagnostics: this.onDidChangeDiagnosticsEmitter.event,
          provideDiagnostics: (document, previousResultId, token) => {
            const provideDiagnostics = (document2, previousResultId2, token2) => {
              const params = {
                identifier: this.options.identifier,
                textDocument: { uri: this.client.code2ProtocolConverter.asUri(document2 instanceof vscode_1.Uri ? document2 : document2.uri) },
                previousResultId: previousResultId2
              };
              if (this.isDisposed === true || !this.client.isRunning()) {
                return { kind: vsdiag.DocumentDiagnosticReportKind.full, items: [] };
              }
              return this.client.sendRequest(vscode_languageserver_protocol_1.DocumentDiagnosticRequest.type, params, token2).then(async (result2) => {
                if (this.isDisposed) {
                  return { kind: vsdiag.DocumentDiagnosticReportKind.full, items: [] };
                }
                if (token2.isCancellationRequested) {
                  throw new vscode_1.CancellationError();
                }
                if (result2 === void 0 || result2 === null) {
                  return { kind: vsdiag.DocumentDiagnosticReportKind.full, items: [] };
                }
                if (result2.kind === vscode_languageserver_protocol_1.DocumentDiagnosticReportKind.Full) {
                  return { kind: vsdiag.DocumentDiagnosticReportKind.full, resultId: result2.resultId, items: await this.client.protocol2CodeConverter.asDiagnostics(result2.items, token2) };
                } else {
                  return { kind: vsdiag.DocumentDiagnosticReportKind.unChanged, resultId: result2.resultId };
                }
              }, (error) => {
                return this.client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentDiagnosticRequest.type, token2, error, { kind: vsdiag.DocumentDiagnosticReportKind.full, items: [] }, true, true);
              });
            };
            const middleware = this.client.middleware;
            return middleware.provideDiagnostics ? middleware.provideDiagnostics(document, previousResultId, token, provideDiagnostics) : provideDiagnostics(document, previousResultId, token);
          }
        };
        if (this.options.workspaceDiagnostics) {
          result.provideWorkspaceDiagnostics = (resultIds, token, resultReporter) => {
            const convertReport = async (report) => {
              if (report.kind === vscode_languageserver_protocol_1.DocumentDiagnosticReportKind.Full) {
                return {
                  kind: vsdiag.DocumentDiagnosticReportKind.full,
                  uri: this.client.protocol2CodeConverter.asUri(report.uri),
                  resultId: report.resultId,
                  version: report.version,
                  items: await this.client.protocol2CodeConverter.asDiagnostics(report.items, token)
                };
              } else {
                return {
                  kind: vsdiag.DocumentDiagnosticReportKind.unChanged,
                  uri: this.client.protocol2CodeConverter.asUri(report.uri),
                  resultId: report.resultId,
                  version: report.version
                };
              }
            };
            const convertPreviousResultIds = (resultIds2) => {
              const converted = [];
              for (const item of resultIds2) {
                converted.push({ uri: this.client.code2ProtocolConverter.asUri(item.uri), value: item.value });
              }
              return converted;
            };
            const provideDiagnostics = (resultIds2, token2, resultReporter2) => {
              const partialResultToken = (0, uuid_1.generateUuid)();
              const disposable = this.client.onProgress(vscode_languageserver_protocol_1.WorkspaceDiagnosticRequest.partialResult, partialResultToken, async (partialResult) => {
                if (partialResult === void 0 || partialResult === null) {
                  resultReporter2(null);
                  return;
                }
                const converted = {
                  items: []
                };
                for (const item of partialResult.items) {
                  try {
                    converted.items.push(await convertReport(item));
                  } catch (error) {
                    this.client.error(`Converting workspace diagnostics failed.`, error);
                  }
                }
                resultReporter2(converted);
              });
              const params = {
                identifier: this.options.identifier,
                previousResultIds: convertPreviousResultIds(resultIds2),
                partialResultToken
              };
              if (this.isDisposed === true || !this.client.isRunning()) {
                return { items: [] };
              }
              return this.client.sendRequest(vscode_languageserver_protocol_1.WorkspaceDiagnosticRequest.type, params, token2).then(async (result2) => {
                if (token2.isCancellationRequested) {
                  return { items: [] };
                }
                const converted = {
                  items: []
                };
                for (const item of result2.items) {
                  converted.items.push(await convertReport(item));
                }
                disposable.dispose();
                resultReporter2(converted);
                return { items: [] };
              }, (error) => {
                disposable.dispose();
                return this.client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentDiagnosticRequest.type, token2, error, { items: [] });
              });
            };
            const middleware = this.client.middleware;
            return middleware.provideWorkspaceDiagnostics ? middleware.provideWorkspaceDiagnostics(resultIds, token, resultReporter, provideDiagnostics) : provideDiagnostics(resultIds, token, resultReporter);
          };
        }
        return result;
      }
      dispose() {
        this.isDisposed = true;
        this.workspaceCancellation?.cancel();
        this.workspaceTimeout?.dispose();
        for (const [key, request] of this.openRequests) {
          if (request.state === RequestStateKind.active) {
            request.tokenSource.cancel();
          }
          this.openRequests.set(key, { state: RequestStateKind.outDated, document: request.document });
        }
        if (this.client.clientOptions.diagnosticCollectionProvider !== void 0) {
          this.client.clientOptions.diagnosticCollectionProvider.dispose(this.diagnostics, features_1.DiagnosticCollectionSource.pull);
        } else {
          this.diagnostics.dispose();
        }
      }
    };
    var BackgroundScheduler = class {
      client;
      diagnosticRequestor;
      lastDocumentToPull;
      documents;
      timeoutHandle;
      // The problem is that there could be outstanding diagnostic requests
      // when we shutdown which when we receive the result will trigger a
      // reschedule. So we remember if the background scheduler got disposed
      // and ignore those re-schedules
      isDisposed;
      constructor(client, diagnosticRequestor) {
        this.client = client;
        this.diagnosticRequestor = diagnosticRequestor;
        this.documents = new vscode_languageserver_protocol_1.LinkedMap();
        this.isDisposed = false;
      }
      add(document) {
        if (this.isDisposed === true) {
          return;
        }
        const key = DocumentOrUri.asKey(document);
        if (this.documents.has(key)) {
          return;
        }
        this.documents.set(key, document, vscode_languageserver_protocol_1.Touch.Last);
        this.lastDocumentToPull = document;
      }
      remove(document) {
        const key = DocumentOrUri.asKey(document);
        this.documents.delete(key);
        if (this.documents.size === 0) {
          this.stop();
          return;
        } else if (key === this.lastDocumentToPullKey()) {
          const before = this.documents.before(key);
          if (before === void 0) {
            this.stop();
          } else {
            this.lastDocumentToPull = before;
          }
        }
      }
      trigger() {
        this.lastDocumentToPull = this.documents.last;
        this.runLoop();
      }
      runLoop() {
        if (this.isDisposed === true) {
          return;
        }
        if (this.documents.size === 0) {
          this.stop();
          return;
        }
        if (this.lastDocumentToPull === void 0) {
          return;
        }
        if (this.timeoutHandle !== void 0) {
          return;
        }
        this.timeoutHandle = (0, vscode_languageserver_protocol_1.RAL)().timer.setTimeout(() => {
          const document = this.documents.first;
          if (document === void 0) {
            return;
          }
          const key = DocumentOrUri.asKey(document);
          this.diagnosticRequestor.pullAsync(document).catch((error) => {
            this.client.error(`Document pull failed for text document ${key}`, error, false);
          }).finally(() => {
            this.timeoutHandle = void 0;
            this.documents.set(key, document, vscode_languageserver_protocol_1.Touch.Last);
            if (key !== this.lastDocumentToPullKey()) {
              this.runLoop();
            }
          });
        }, 500);
      }
      dispose() {
        this.isDisposed = true;
        this.stop();
        this.documents.clear();
        this.lastDocumentToPull = void 0;
      }
      stop() {
        this.timeoutHandle?.dispose();
        this.timeoutHandle = void 0;
        this.lastDocumentToPull = void 0;
      }
      lastDocumentToPullKey() {
        return this.lastDocumentToPull !== void 0 ? DocumentOrUri.asKey(this.lastDocumentToPull) : void 0;
      }
    };
    var DiagnosticFeatureProviderImpl = class {
      disposable;
      diagnosticRequestor;
      activeTextDocument;
      backgroundScheduler;
      constructor(client, visibleDocuments, options) {
        const diagnosticPullOptions = Object.assign({ onChange: false, onSave: false, onFocus: false }, client.clientOptions.diagnosticPullOptions);
        const documentSelector = client.protocol2CodeConverter.asDocumentSelector(options.documentSelector);
        const disposables = [];
        const matchFilter = (filter, resource) => {
          if (typeof filter === "string") {
            return false;
          }
          if (filter.language !== void 0 && filter.language !== "*") {
            return false;
          }
          if (filter.scheme !== void 0 && filter.scheme !== "*" && filter.scheme !== resource.scheme) {
            return false;
          }
          if (filter.pattern !== void 0 && !(0, globPattern_1.matchGlobPattern)(filter.pattern, resource)) {
            return false;
          }
          return true;
        };
        const matchResource = (resource) => {
          const selector = options.documentSelector;
          if (diagnosticPullOptions.match !== void 0) {
            return diagnosticPullOptions.match(selector, resource);
          }
          for (const filter of selector) {
            if (!vscode_languageserver_protocol_1.TextDocumentFilter.is(filter)) {
              continue;
            }
            if (matchFilter(filter, resource)) {
              return true;
            }
          }
          return false;
        };
        const matches = (document) => {
          return document instanceof vscode_1.Uri ? matchResource(document) : vscode_1.languages.match(documentSelector, document) > 0 && visibleDocuments.isVisible(document);
        };
        const matchesCell = (cell) => {
          return vscode_1.languages.match(documentSelector, cell.document) > 0 && visibleDocuments.isVisible(cell.notebook.uri);
        };
        const isActiveDocument = (document) => {
          return document instanceof vscode_1.Uri ? this.activeTextDocument?.uri.toString() === document.toString() : this.activeTextDocument === document;
        };
        this.diagnosticRequestor = new DiagnosticRequestor(client, visibleDocuments, options);
        this.backgroundScheduler = new BackgroundScheduler(client, this.diagnosticRequestor);
        const addToBackgroundIfNeeded = (document) => {
          if (!matches(document) || !options.interFileDependencies || isActiveDocument(document) || diagnosticPullOptions.onChange === false) {
            return;
          }
          this.backgroundScheduler.add(document);
        };
        const considerDocument = (textDocument, mode) => {
          return (diagnosticPullOptions.filter === void 0 || !diagnosticPullOptions.filter(textDocument, mode)) && this.diagnosticRequestor.knows(PullState.document, textDocument);
        };
        this.activeTextDocument = vscode_1.window.activeTextEditor?.document;
        disposables.push(vscode_1.window.onDidChangeActiveTextEditor((editor) => {
          const oldActive = this.activeTextDocument;
          this.activeTextDocument = editor?.document;
          if (oldActive !== void 0) {
            addToBackgroundIfNeeded(oldActive);
          }
          if (this.activeTextDocument !== void 0) {
            this.backgroundScheduler.remove(this.activeTextDocument);
            if (diagnosticPullOptions.onFocus === true && matches(this.activeTextDocument) && considerDocument(this.activeTextDocument, DiagnosticPullMode.onFocus)) {
              this.diagnosticRequestor.pull(this.activeTextDocument);
            }
          }
        }));
        const openFeature = client.getFeature(vscode_languageserver_protocol_1.DidOpenTextDocumentNotification.method);
        disposables.push(openFeature.onNotificationSent((event) => {
          const textDocument = event.textDocument;
          if (this.diagnosticRequestor.knowsSameVersion(PullState.document, textDocument)) {
            return;
          }
          if (matches(textDocument)) {
            this.diagnosticRequestor.pull(textDocument, () => {
              addToBackgroundIfNeeded(textDocument);
            });
          }
        }));
        const notebookFeature = client.getFeature(vscode_languageserver_protocol_1.NotebookDocumentSyncRegistrationType.method);
        disposables.push(notebookFeature.onOpenNotificationSent((event) => {
          for (const cell of event.getCells()) {
            if (matchesCell(cell)) {
              this.diagnosticRequestor.pull(cell.document, () => {
                addToBackgroundIfNeeded(cell.document);
              });
            }
          }
        }));
        disposables.push(visibleDocuments.onOpen((opened) => {
          for (const resource of opened) {
            if (this.diagnosticRequestor.knows(PullState.document, resource)) {
              continue;
            }
            const uriStr = resource.toString();
            let textDocument;
            for (const item of vscode_1.workspace.textDocuments) {
              if (uriStr === item.uri.toString()) {
                textDocument = item;
                break;
              }
            }
            if (textDocument !== void 0 && matches(textDocument)) {
              this.diagnosticRequestor.pull(textDocument, () => {
                addToBackgroundIfNeeded(textDocument);
              });
            }
          }
        }));
        const pulledTextDocuments = /* @__PURE__ */ new Set();
        for (const textDocument of vscode_1.workspace.textDocuments) {
          if (matches(textDocument)) {
            this.diagnosticRequestor.pull(textDocument, () => {
              addToBackgroundIfNeeded(textDocument);
            });
            pulledTextDocuments.add(textDocument.uri.toString());
          }
        }
        for (const notebookDocument of vscode_1.workspace.notebookDocuments) {
          for (const cell of notebookDocument.getCells()) {
            if (matchesCell(cell)) {
              this.diagnosticRequestor.pull(cell.document, () => {
                addToBackgroundIfNeeded(cell.document);
              });
              pulledTextDocuments.add(cell.document.uri.toString());
            }
          }
        }
        if (diagnosticPullOptions.onTabs === true) {
          for (const resource of visibleDocuments.getResources()) {
            if (!pulledTextDocuments.has(resource.toString()) && matches(resource)) {
              this.diagnosticRequestor.pull(resource, () => {
                addToBackgroundIfNeeded(resource);
              });
            }
          }
        }
        if (diagnosticPullOptions.onChange === true) {
          const changeFeature = client.getFeature(vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.method);
          disposables.push(changeFeature.onNotificationSent(async (event) => {
            const textDocument = event.textDocument;
            if (considerDocument(textDocument, DiagnosticPullMode.onType)) {
              this.diagnosticRequestor.pull(textDocument, () => {
                this.backgroundScheduler.trigger();
              });
            }
          }));
          disposables.push(notebookFeature.onChangeNotificationSent(async (event) => {
            const textEvents = event.cells?.textContent || [];
            const changedCells = textEvents.map((c) => event.notebook.getCells().find((cell) => cell.document.uri.toString() === c.document.uri.toString()));
            for (const cell of changedCells) {
              if (cell && matchesCell(cell)) {
                this.diagnosticRequestor.pull(cell.document, () => {
                  this.backgroundScheduler.trigger();
                });
              }
            }
            const closedCells = event.cells?.structure?.didClose || [];
            for (const cell of closedCells) {
              this.diagnosticRequestor.forgetDocument(cell.document);
            }
            const openedCells = event.cells?.structure?.didOpen || [];
            for (const cell of openedCells) {
              if (matchesCell(cell)) {
                this.diagnosticRequestor.pull(cell.document, () => {
                  this.backgroundScheduler.trigger();
                });
              }
            }
          }));
        }
        if (diagnosticPullOptions.onSave === true) {
          const saveFeature = client.getFeature(vscode_languageserver_protocol_1.DidSaveTextDocumentNotification.method);
          disposables.push(saveFeature.onNotificationSent((event) => {
            const textDocument = event.textDocument;
            if (considerDocument(textDocument, DiagnosticPullMode.onSave)) {
              this.diagnosticRequestor.pull(event.textDocument);
            }
          }));
          disposables.push(notebookFeature.onSaveNotificationSent((event) => {
            for (const cell of event.getCells()) {
              if (matchesCell(cell)) {
                this.diagnosticRequestor.pull(cell.document);
              }
            }
          }));
        }
        const closeFeature = client.getFeature(vscode_languageserver_protocol_1.DidCloseTextDocumentNotification.method);
        disposables.push(closeFeature.onAboutToSendNotification((event) => {
          this.cleanUpDocument(event.textDocument);
        }));
        disposables.push(notebookFeature.onCloseNotificationSent((event) => {
          for (const cell of event.getCells()) {
            this.cleanUpDocument(cell.document);
          }
        }));
        disposables.push(visibleDocuments.onClose((closed) => {
          for (const document of closed) {
            this.cleanUpDocument(document);
          }
        }));
        this.diagnosticRequestor.onDidChangeDiagnosticsEmitter.event(() => {
          for (const textDocument of vscode_1.workspace.textDocuments) {
            if (matches(textDocument)) {
              this.diagnosticRequestor.pull(textDocument);
            }
          }
        });
        if (options.workspaceDiagnostics === true && options.identifier !== "da348dc5-c30a-4515-9d98-31ff3be38d14") {
          this.diagnosticRequestor.pullWorkspace();
        }
        this.disposable = vscode_1.Disposable.from(...disposables, this.backgroundScheduler, this.diagnosticRequestor);
      }
      get onDidChangeDiagnosticsEmitter() {
        return this.diagnosticRequestor.onDidChangeDiagnosticsEmitter;
      }
      get diagnostics() {
        return this.diagnosticRequestor.provider;
      }
      forget(document) {
        this.cleanUpDocument(document);
      }
      cleanUpDocument(document) {
        this.backgroundScheduler.remove(document);
        if (this.diagnosticRequestor.knows(PullState.document, document)) {
          this.diagnosticRequestor.forgetDocument(document);
        }
      }
    };
    var DiagnosticFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentDiagnosticRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = ensure(ensure(capabilities, "textDocument"), "diagnostic");
        capability.relatedInformation = true;
        capability.tagSupport = { valueSet: [vscode_languageserver_protocol_1.DiagnosticTag.Unnecessary, vscode_languageserver_protocol_1.DiagnosticTag.Deprecated] };
        capability.codeDescriptionSupport = true;
        capability.dataSupport = true;
        capability.dynamicRegistration = true;
        capability.relatedDocumentSupport = false;
        capability.markupMessageSupport = false;
        ensure(ensure(capabilities, "workspace"), "diagnostics").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.DiagnosticRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeDiagnosticsEmitter.fire();
          }
        });
        const [id, options] = this.getRegistration(documentSelector, capabilities.diagnosticProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      clear() {
        super.clear();
      }
      refresh() {
        for (const provider of this.getAllProviders()) {
          provider.onDidChangeDiagnosticsEmitter.fire();
        }
      }
      registerLanguageProvider(options) {
        const provider = new DiagnosticFeatureProviderImpl(this._client, this._client.visibleDocuments, options);
        return [provider.disposable, provider];
      }
    };
    exports2.DiagnosticFeature = DiagnosticFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/notebook.js
var require_notebook = __commonJS({
  "node_modules/vscode-languageclient/lib/common/notebook.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.NotebookDocumentSyncFeature = void 0;
    var vscode2 = __importStar(require("vscode"));
    var proto = __importStar(require_api2());
    var UUID = __importStar(require_uuid());
    var Is2 = __importStar(require_is());
    var globPattern_1 = require_globPattern();
    function ensure(target, key) {
      if (target[key] === void 0) {
        target[key] = {};
      }
      return target[key];
    }
    var Converter;
    (function(Converter2) {
      let c2p;
      (function(c2p2) {
        function asVersionedNotebookDocumentIdentifier(notebookDocument, base) {
          return {
            version: notebookDocument.version,
            uri: base.asUri(notebookDocument.uri)
          };
        }
        c2p2.asVersionedNotebookDocumentIdentifier = asVersionedNotebookDocumentIdentifier;
        function asNotebookDocument(notebookDocument, cells, base) {
          const result = proto.NotebookDocument.create(base.asUri(notebookDocument.uri), notebookDocument.notebookType, notebookDocument.version, asNotebookCells(cells, base));
          if (Object.keys(notebookDocument.metadata).length > 0) {
            result.metadata = asMetadata(notebookDocument.metadata);
          }
          return result;
        }
        c2p2.asNotebookDocument = asNotebookDocument;
        function asNotebookCells(cells, base) {
          return cells.map((cell) => asNotebookCell(cell, base));
        }
        c2p2.asNotebookCells = asNotebookCells;
        function asMetadata(metadata) {
          const seen = /* @__PURE__ */ new Set();
          return deepCopy(seen, metadata);
        }
        c2p2.asMetadata = asMetadata;
        function asNotebookCell(cell, base) {
          const result = proto.NotebookCell.create(asNotebookCellKind(cell.kind), base.asUri(cell.document.uri));
          if (Object.keys(cell.metadata).length > 0) {
            result.metadata = asMetadata(cell.metadata);
          }
          if (cell.executionSummary !== void 0 && (Is2.number(cell.executionSummary.executionOrder) && Is2.boolean(cell.executionSummary.success))) {
            result.executionSummary = {
              executionOrder: cell.executionSummary.executionOrder,
              success: cell.executionSummary.success
            };
          }
          return result;
        }
        c2p2.asNotebookCell = asNotebookCell;
        function asNotebookCellKind(kind) {
          switch (kind) {
            case vscode2.NotebookCellKind.Markup:
              return proto.NotebookCellKind.Markup;
            case vscode2.NotebookCellKind.Code:
              return proto.NotebookCellKind.Code;
          }
        }
        function deepCopy(seen, value) {
          if (seen.has(value)) {
            throw new Error(`Can't deep copy cyclic structures.`);
          }
          if (Array.isArray(value)) {
            const result = [];
            for (const elem of value) {
              if (elem !== null && typeof elem === "object" || Array.isArray(elem)) {
                result.push(deepCopy(seen, elem));
              } else {
                if (elem instanceof RegExp) {
                  throw new Error(`Can't transfer regular expressions to the server`);
                }
                result.push(elem);
              }
            }
            return result;
          } else {
            const props = Object.keys(value);
            const result = /* @__PURE__ */ Object.create(null);
            for (const prop of props) {
              const elem = value[prop];
              if (elem !== null && typeof elem === "object" || Array.isArray(elem)) {
                result[prop] = deepCopy(seen, elem);
              } else {
                if (elem instanceof RegExp) {
                  throw new Error(`Can't transfer regular expressions to the server`);
                }
                result[prop] = elem;
              }
            }
            return result;
          }
        }
        function asTextContentChange(event, base) {
          const params = base.asChangeTextDocumentParams(event, event.document.uri, event.document.version);
          return { document: params.textDocument, changes: params.contentChanges };
        }
        c2p2.asTextContentChange = asTextContentChange;
        function asNotebookDocumentChangeEvent(event, base) {
          const result = /* @__PURE__ */ Object.create(null);
          if (event.metadata) {
            result.metadata = Converter2.c2p.asMetadata(event.metadata);
          }
          if (event.cells !== void 0) {
            const cells = /* @__PURE__ */ Object.create(null);
            const changedCells = event.cells;
            if (changedCells.structure) {
              cells.structure = {
                array: {
                  start: changedCells.structure.array.start,
                  deleteCount: changedCells.structure.array.deleteCount,
                  cells: changedCells.structure.array.cells !== void 0 ? changedCells.structure.array.cells.map((cell) => Converter2.c2p.asNotebookCell(cell, base)) : void 0
                },
                didOpen: changedCells.structure.didOpen !== void 0 ? changedCells.structure.didOpen.map((cell) => base.asOpenTextDocumentParams(cell.document).textDocument) : void 0,
                didClose: changedCells.structure.didClose !== void 0 ? changedCells.structure.didClose.map((cell) => base.asCloseTextDocumentParams(cell.document).textDocument) : void 0
              };
            }
            if (changedCells.data !== void 0) {
              cells.data = changedCells.data.map((cell) => Converter2.c2p.asNotebookCell(cell, base));
            }
            if (changedCells.textContent !== void 0) {
              cells.textContent = changedCells.textContent.map((event2) => Converter2.c2p.asTextContentChange(event2, base));
            }
            if (Object.keys(cells).length > 0) {
              result.cells = cells;
            }
          }
          return result;
        }
        c2p2.asNotebookDocumentChangeEvent = asNotebookDocumentChangeEvent;
      })(c2p = Converter2.c2p || (Converter2.c2p = {}));
    })(Converter || (Converter = {}));
    var $NotebookCell;
    (function($NotebookCell2) {
      function computeDiff(originalCells, modifiedCells, compareMetadata) {
        const originalLength = originalCells.length;
        const modifiedLength = modifiedCells.length;
        let startIndex = 0;
        while (startIndex < modifiedLength && startIndex < originalLength && equals(originalCells[startIndex], modifiedCells[startIndex], compareMetadata)) {
          startIndex++;
        }
        if (startIndex < modifiedLength && startIndex < originalLength) {
          let originalEndIndex = originalLength - 1;
          let modifiedEndIndex = modifiedLength - 1;
          while (originalEndIndex >= 0 && modifiedEndIndex >= 0 && equals(originalCells[originalEndIndex], modifiedCells[modifiedEndIndex], compareMetadata)) {
            originalEndIndex--;
            modifiedEndIndex--;
          }
          const deleteCount = originalEndIndex + 1 - startIndex;
          const newCells = startIndex === modifiedEndIndex + 1 ? void 0 : modifiedCells.slice(startIndex, modifiedEndIndex + 1);
          return newCells !== void 0 ? { start: startIndex, deleteCount, cells: newCells } : { start: startIndex, deleteCount };
        } else if (startIndex < modifiedLength) {
          return { start: startIndex, deleteCount: 0, cells: modifiedCells.slice(startIndex) };
        } else if (startIndex < originalLength) {
          return { start: startIndex, deleteCount: originalLength - startIndex };
        } else {
          return void 0;
        }
      }
      $NotebookCell2.computeDiff = computeDiff;
      function equals(one, other, compareMetaData = true) {
        if (one.kind !== other.kind || one.document.uri.toString() !== other.document.uri.toString() || one.document.languageId !== other.document.languageId || !equalsExecution(one.executionSummary, other.executionSummary)) {
          return false;
        }
        return !compareMetaData || compareMetaData && equalsMetadata(one.metadata, other.metadata);
      }
      function equalsExecution(one, other) {
        if (one === other) {
          return true;
        }
        if (one === void 0 || other === void 0) {
          return false;
        }
        return one.executionOrder === other.executionOrder && one.success === other.success && equalsTiming(one.timing, other.timing);
      }
      function equalsTiming(one, other) {
        if (one === other) {
          return true;
        }
        if (one === void 0 || other === void 0) {
          return false;
        }
        return one.startTime === other.startTime && one.endTime === other.endTime;
      }
      function equalsMetadata(one, other) {
        if (one === other) {
          return true;
        }
        if (one === null || one === void 0 || other === null || other === void 0) {
          return false;
        }
        if (typeof one !== typeof other) {
          return false;
        }
        if (typeof one !== "object") {
          return false;
        }
        const oneArray = Array.isArray(one);
        const otherArray = Array.isArray(other);
        if (oneArray !== otherArray) {
          return false;
        }
        if (oneArray && otherArray) {
          if (one.length !== other.length) {
            return false;
          }
          for (let i = 0; i < one.length; i++) {
            if (!equalsMetadata(one[i], other[i])) {
              return false;
            }
          }
        }
        if (isObjectLiteral(one) && isObjectLiteral(other)) {
          const oneKeys = Object.keys(one);
          const otherKeys = Object.keys(other);
          if (oneKeys.length !== otherKeys.length) {
            return false;
          }
          oneKeys.sort();
          otherKeys.sort();
          if (!equalsMetadata(oneKeys, otherKeys)) {
            return false;
          }
          for (let i = 0; i < oneKeys.length; i++) {
            const prop = oneKeys[i];
            if (!equalsMetadata(one[prop], other[prop])) {
              return false;
            }
          }
          return true;
        }
        return false;
      }
      function isObjectLiteral(value) {
        return value !== null && typeof value === "object";
      }
      $NotebookCell2.isObjectLiteral = isObjectLiteral;
    })($NotebookCell || ($NotebookCell = {}));
    var $NotebookDocumentFilter;
    (function($NotebookDocumentFilter2) {
      function matchNotebook(filter, notebookDocument) {
        if (typeof filter === "string") {
          return filter === "*" || notebookDocument.notebookType === filter;
        }
        if (filter.notebookType !== void 0 && filter.notebookType !== "*" && notebookDocument.notebookType !== filter.notebookType) {
          return false;
        }
        const uri = notebookDocument.uri;
        if (filter.scheme !== void 0 && filter.scheme !== "*" && uri.scheme !== filter.scheme) {
          return false;
        }
        if (filter.pattern !== void 0) {
          if (!(0, globPattern_1.matchGlobPattern)(filter.pattern, uri)) {
            return false;
          }
        }
        return true;
      }
      $NotebookDocumentFilter2.matchNotebook = matchNotebook;
    })($NotebookDocumentFilter || ($NotebookDocumentFilter = {}));
    var $NotebookDocumentSyncOptions;
    (function($NotebookDocumentSyncOptions2) {
      function asDocumentSelector(options) {
        const selector = options.notebookSelector;
        const result = [];
        for (const element of selector) {
          const notebookType = (typeof element.notebook === "string" ? element.notebook : element.notebook?.notebookType) ?? "*";
          const scheme = typeof element.notebook === "string" ? void 0 : element.notebook?.scheme;
          const pattern = typeof element.notebook === "string" ? void 0 : element.notebook?.pattern;
          if (element.cells !== void 0) {
            for (const cell of element.cells) {
              result.push(asDocumentFilter(notebookType, scheme, pattern, cell.language));
            }
          } else {
            result.push(asDocumentFilter(notebookType, scheme, pattern, void 0));
          }
        }
        return result;
      }
      $NotebookDocumentSyncOptions2.asDocumentSelector = asDocumentSelector;
      function asDocumentFilter(notebookType, scheme, pattern, language) {
        return scheme === void 0 && pattern === void 0 ? { notebook: notebookType, language } : { notebook: { notebookType, scheme, pattern }, language };
      }
    })($NotebookDocumentSyncOptions || ($NotebookDocumentSyncOptions = {}));
    var SyncInfo;
    (function(SyncInfo2) {
      function create(cells) {
        return {
          cells,
          uris: new Set(cells.map((cell) => cell.document.uri.toString()))
        };
      }
      SyncInfo2.create = create;
    })(SyncInfo || (SyncInfo = {}));
    var NotebookDocumentSyncFeatureProvider = class {
      client;
      options;
      notebookSyncInfo;
      notebookDidOpen;
      disposables;
      selector;
      onChangeNotificationSent;
      onOpenNotificationSent;
      onCloseNotificationSent;
      onSaveNotificationSent;
      constructor(client, options, onChangeNotificationSent, onOpenNotificationSent, onCloseNotificationSent, onSaveNotificationSent) {
        this.client = client;
        this.options = options;
        this.notebookSyncInfo = /* @__PURE__ */ new Map();
        this.notebookDidOpen = /* @__PURE__ */ new Set();
        this.disposables = [];
        this.selector = client.protocol2CodeConverter.asDocumentSelector($NotebookDocumentSyncOptions.asDocumentSelector(options));
        this.onChangeNotificationSent = onChangeNotificationSent;
        this.onOpenNotificationSent = onOpenNotificationSent;
        this.onCloseNotificationSent = onCloseNotificationSent;
        this.onSaveNotificationSent = onSaveNotificationSent;
        vscode2.workspace.onDidOpenNotebookDocument((notebookDocument) => {
          this.notebookDidOpen.add(notebookDocument.uri.toString());
          this.didOpen(notebookDocument);
        }, void 0, this.disposables);
        for (const notebookDocument of vscode2.workspace.notebookDocuments) {
          this.notebookDidOpen.add(notebookDocument.uri.toString());
          this.didOpen(notebookDocument);
        }
        vscode2.workspace.onDidChangeNotebookDocument((event) => this.didChangeNotebookDocument(event), void 0, this.disposables);
        if (this.options.save === true) {
          vscode2.workspace.onDidSaveNotebookDocument((notebookDocument) => this.didSave(notebookDocument), void 0, this.disposables);
        }
        vscode2.workspace.onDidCloseNotebookDocument((notebookDocument) => {
          this.didClose(notebookDocument);
          this.notebookDidOpen.delete(notebookDocument.uri.toString());
        }, void 0, this.disposables);
      }
      getState() {
        for (const notebook of vscode2.workspace.notebookDocuments) {
          const matchingCells = this.getMatchingCellsConsideringSyncInfo(notebook);
          if (matchingCells !== void 0) {
            return { kind: "document", id: "$internal", registrations: true, matches: true };
          }
        }
        return { kind: "document", id: "$internal", registrations: true, matches: false };
      }
      get mode() {
        return "notebook";
      }
      handles(textDocument) {
        if (vscode2.languages.match(this.selector, textDocument) > 0) {
          return true;
        }
        const key = textDocument.uri.toString();
        for (const syncInfo of this.notebookSyncInfo.values()) {
          if (syncInfo.uris.has(key)) {
            return true;
          }
        }
        return false;
      }
      didOpenNotebookCellTextDocument(notebookDocument, cell) {
        if (vscode2.languages.match(this.selector, cell.document) === 0) {
          return;
        }
        if (!this.notebookDidOpen.has(notebookDocument.uri.toString())) {
          return;
        }
        const syncInfo = this.getSyncInfo(notebookDocument);
        const cellMatches = this.cellMatches(notebookDocument, cell);
        if (syncInfo !== void 0) {
          const cellIsSynced = syncInfo.uris.has(cell.document.uri.toString());
          if (cellMatches && cellIsSynced || !cellMatches && !cellIsSynced) {
            return;
          }
          if (cellMatches) {
            const matchingCells = this.mergeCells(notebookDocument, syncInfo, [cell]);
            if (matchingCells !== void 0) {
              const event = this.asNotebookDocumentChangeEvent(notebookDocument, void 0, syncInfo, matchingCells);
              if (event !== void 0) {
                this.doSendChange(event, matchingCells).catch(() => {
                });
              }
            }
          }
        } else {
          if (cellMatches) {
            this.doSendOpen(notebookDocument, [cell]).catch(() => {
            });
          }
        }
      }
      didChangeNotebookCellTextDocument(notebookDocument, cell, event) {
        if (vscode2.languages.match(this.selector, event.document) === 0) {
          return;
        }
        const syncInfo = this.getSyncInfo(notebookDocument);
        if (syncInfo === void 0 || !syncInfo.uris.has(cell.document.uri.toString())) {
          return;
        }
        this.doSendChange({
          notebook: notebookDocument,
          cells: { textContent: [event] }
        }, syncInfo.cells).catch(() => {
        });
      }
      didCloseNotebookCellTextDocument(notebookDocument, cell) {
        const syncInfo = this.getSyncInfo(notebookDocument);
        if (syncInfo === void 0) {
          return;
        }
        const cellUri = cell.document.uri;
        const index = syncInfo.cells.findIndex((item) => item.document.uri.toString() === cellUri.toString());
        if (index === -1) {
          return;
        }
        if (index === 0 && syncInfo.cells.length === 1) {
          this.doSendClose(notebookDocument, syncInfo.cells).catch(() => {
          });
        } else {
          const newCells = syncInfo.cells.slice();
          const deleted = newCells.splice(index, 1);
          this.doSendChange({
            notebook: notebookDocument,
            cells: {
              structure: {
                array: { start: index, deleteCount: 1 },
                didClose: deleted
              }
            }
          }, newCells).catch(() => {
          });
        }
      }
      dispose() {
        for (const disposable of this.disposables) {
          disposable.dispose();
        }
      }
      didOpen(notebookDocument, matchingCells, syncInfo = this.getSyncInfo(notebookDocument)) {
        if (syncInfo !== void 0) {
          if (matchingCells === void 0) {
            matchingCells = syncInfo.cells.slice();
          }
          if (matchingCells !== void 0) {
            const event = this.asNotebookDocumentChangeEvent(notebookDocument, void 0, syncInfo, matchingCells);
            if (event !== void 0) {
              this.doSendChange(event, matchingCells).catch(() => {
              });
            }
          } else {
            this.doSendClose(notebookDocument, []).catch(() => {
            });
          }
        } else {
          matchingCells = this.getMatchingCells(notebookDocument);
          if (matchingCells === void 0) {
            return;
          }
          this.doSendOpen(notebookDocument, matchingCells).catch(() => {
          });
        }
      }
      didChangeNotebookDocument(event) {
        const notebookDocument = event.notebook;
        const syncInfo = this.getSyncInfo(notebookDocument);
        if (syncInfo === void 0) {
          if (event.contentChanges.length === 0) {
            return;
          }
          const cells = this.getMatchingCells(notebookDocument);
          if (cells === void 0) {
            return;
          }
          this.didOpen(notebookDocument, cells, syncInfo);
        } else {
          const cells = this.getMatchingCellsFromEvent(notebookDocument, syncInfo, event);
          if (cells === void 0) {
            this.didClose(notebookDocument, syncInfo);
            return;
          }
          const newEvent = this.asNotebookDocumentChangeEvent(event.notebook, event, syncInfo, cells);
          if (newEvent !== void 0) {
            this.doSendChange(newEvent, cells).catch(() => {
            });
          }
        }
      }
      didSave(notebookDocument) {
        const syncInfo = this.getSyncInfo(notebookDocument);
        if (syncInfo === void 0) {
          return;
        }
        this.doSendSave(notebookDocument).catch(() => {
        });
      }
      didClose(notebookDocument, syncInfo = this.getSyncInfo(notebookDocument)) {
        if (syncInfo === void 0) {
          return;
        }
        const syncedCells = notebookDocument.getCells().filter((cell) => syncInfo.uris.has(cell.document.uri.toString()));
        this.doSendClose(notebookDocument, syncedCells).catch(() => {
        });
      }
      async sendDidOpenNotebookDocument(notebookDocument) {
        const syncInfo = this.getSyncInfo(notebookDocument);
        if (syncInfo !== void 0) {
          throw new Error(`Notebook document ${notebookDocument.uri.toString()} is already open`);
        }
        const cells = this.getMatchingCells(notebookDocument);
        if (cells === void 0) {
          return;
        }
        return this.doSendOpen(notebookDocument, cells);
      }
      async doSendOpen(notebookDocument, cells) {
        const send = async (notebookDocument2, cells2) => {
          const cellDocuments = cells2.map((cell) => this.client.code2ProtocolConverter.asTextDocumentItem(cell.document));
          try {
            await this.client.sendNotification(proto.DidOpenNotebookDocumentNotification.type, {
              notebookDocument: Converter.c2p.asNotebookDocument(notebookDocument2, cells2, this.client.code2ProtocolConverter),
              cellTextDocuments: cellDocuments
            });
            this.onOpenNotificationSent.fire(notebookDocument2);
          } catch (error) {
            this.client.error("Sending DidOpenNotebookDocumentNotification failed", error);
            throw error;
          }
        };
        const middleware = this.client.middleware?.notebooks;
        this.notebookSyncInfo.set(notebookDocument.uri.toString(), SyncInfo.create(cells));
        return middleware?.didOpen !== void 0 ? middleware.didOpen(notebookDocument, cells, send) : send(notebookDocument, cells);
      }
      async sendDidChangeNotebookDocument(event) {
        const cells = this.getMatchingCellsFromSyncInfo(event.notebook);
        if (cells === void 0) {
          throw new Error(`Received changed event for un-synced notebook ${event.notebook.uri.toString()}`);
        }
        return this.doSendChange(event, cells);
      }
      async doSendChange(event, cells) {
        const send = async (event2) => {
          try {
            await this.client.sendNotification(proto.DidChangeNotebookDocumentNotification.type, {
              notebookDocument: Converter.c2p.asVersionedNotebookDocumentIdentifier(event2.notebook, this.client.code2ProtocolConverter),
              change: Converter.c2p.asNotebookDocumentChangeEvent(event2, this.client.code2ProtocolConverter)
            });
            this.onChangeNotificationSent.fire(event2);
          } catch (error) {
            this.client.error("Sending DidChangeNotebookDocumentNotification failed", error);
            throw error;
          }
        };
        const middleware = this.client.middleware?.notebooks;
        if (event.cells?.structure !== void 0) {
          this.notebookSyncInfo.set(event.notebook.uri.toString(), SyncInfo.create(cells));
        }
        return middleware?.didChange !== void 0 ? middleware?.didChange(event, send) : send(event);
      }
      async sendDidSaveNotebookDocument(notebookDocument) {
        return this.doSendSave(notebookDocument);
      }
      async doSendSave(notebookDocument) {
        const send = async (notebookDocument2) => {
          try {
            await this.client.sendNotification(proto.DidSaveNotebookDocumentNotification.type, {
              notebookDocument: { uri: this.client.code2ProtocolConverter.asUri(notebookDocument2.uri) }
            });
            this.onSaveNotificationSent.fire(notebookDocument2);
          } catch (error) {
            this.client.error("Sending DidSaveNotebookDocumentNotification failed", error);
            throw error;
          }
        };
        const middleware = this.client.middleware?.notebooks;
        return middleware?.didSave !== void 0 ? middleware.didSave(notebookDocument, send) : send(notebookDocument);
      }
      async sendDidCloseNotebookDocument(notebookDocument) {
        const cells = this.getMatchingCellsFromSyncInfo(notebookDocument);
        if (cells === void 0) {
          throw new Error(`Received close event for un-synced notebook ${notebookDocument.uri.toString()}`);
        }
        return this.doSendClose(notebookDocument, cells);
      }
      async doSendClose(notebookDocument, cells) {
        const send = async (notebookDocument2, cells2) => {
          try {
            await this.client.sendNotification(proto.DidCloseNotebookDocumentNotification.type, {
              notebookDocument: { uri: this.client.code2ProtocolConverter.asUri(notebookDocument2.uri) },
              cellTextDocuments: cells2.map((cell) => this.client.code2ProtocolConverter.asTextDocumentIdentifier(cell.document))
            });
            this.onCloseNotificationSent.fire(notebookDocument2);
          } catch (error) {
            this.client.error("Sending DidCloseNotebookDocumentNotification failed", error);
            throw error;
          }
        };
        const middleware = this.client.middleware?.notebooks;
        this.notebookSyncInfo.delete(notebookDocument.uri.toString());
        return middleware?.didClose !== void 0 ? middleware.didClose(notebookDocument, cells, send) : send(notebookDocument, cells);
      }
      getSynchronizedCells(notebookDocument) {
        const syncInfo = this.getSyncInfo(notebookDocument);
        return syncInfo?.cells;
      }
      asNotebookDocumentChangeEvent(notebook, event, syncInfo, matchingCells) {
        if (event !== void 0 && event.notebook !== notebook) {
          throw new Error("Notebook must be identical");
        }
        const result = {
          notebook
        };
        if (event?.metadata !== void 0) {
          result.metadata = Converter.c2p.asMetadata(event.metadata);
        }
        let matchingCellsSet;
        if (event?.cellChanges !== void 0 && event.cellChanges.length > 0) {
          const data = [];
          matchingCellsSet = new Set(matchingCells.map((cell) => cell.document.uri.toString()));
          for (const cellChange of event.cellChanges) {
            if (matchingCellsSet.has(cellChange.cell.document.uri.toString()) && (cellChange.executionSummary !== void 0 || cellChange.metadata !== void 0)) {
              data.push(cellChange.cell);
            }
          }
          if (data.length > 0) {
            result.cells = result.cells ?? {};
            result.cells.data = data;
          }
        }
        if ((event?.contentChanges !== void 0 && event.contentChanges.length > 0 || event === void 0) && syncInfo !== void 0 && matchingCells !== void 0) {
          const oldCells = syncInfo.cells;
          const newCells = matchingCells;
          const diff = $NotebookCell.computeDiff(oldCells, newCells, false);
          let addedCells;
          let removedCells;
          if (diff !== void 0) {
            addedCells = diff.cells === void 0 ? /* @__PURE__ */ new Map() : new Map(diff.cells.map((cell) => [cell.document.uri.toString(), cell]));
            removedCells = diff.deleteCount === 0 ? /* @__PURE__ */ new Map() : new Map(oldCells.slice(diff.start, diff.start + diff.deleteCount).map((cell) => [cell.document.uri.toString(), cell]));
            for (const key of Array.from(removedCells.keys())) {
              if (addedCells.has(key)) {
                removedCells.delete(key);
                addedCells.delete(key);
              }
            }
            result.cells = result.cells ?? {};
            const didOpen = [];
            const didClose = [];
            if (addedCells.size > 0 || removedCells.size > 0) {
              for (const cell of addedCells.values()) {
                didOpen.push(cell);
              }
              for (const cell of removedCells.values()) {
                didClose.push(cell);
              }
            }
            result.cells.structure = {
              array: diff,
              didOpen,
              didClose
            };
          }
        }
        return Object.keys(result).length > 1 ? result : void 0;
      }
      getMatchingCells(notebookDocument, cells = notebookDocument.getCells()) {
        if (this.options.notebookSelector === void 0) {
          return void 0;
        }
        for (const item of this.options.notebookSelector) {
          if (item.notebook === void 0 || $NotebookDocumentFilter.matchNotebook(item.notebook, notebookDocument)) {
            const filtered = this.filterCells(notebookDocument, cells, item.cells);
            return filtered.length === 0 ? void 0 : filtered;
          }
        }
        return void 0;
      }
      getMatchingCellsFromEvent(notebookDocument, syncInfo, event) {
        if (this.options.notebookSelector === void 0) {
          return void 0;
        }
        let selector;
        for (const item of this.options.notebookSelector) {
          if (item.notebook === void 0 || $NotebookDocumentFilter.matchNotebook(item.notebook, notebookDocument)) {
            selector = item;
            break;
          }
        }
        if (selector === void 0) {
          return void 0;
        }
        if ((event.cellChanges === void 0 || event.cellChanges.length === 0) && (event.contentChanges === void 0 || event.contentChanges.length === 0)) {
          return syncInfo.cells;
        }
        let cells;
        if (event.cellChanges !== void 0 && event.cellChanges.length > 0) {
          const changedCells = event.cellChanges.map((item) => item.cell);
          const filtered = this.filterCells(notebookDocument, changedCells, selector.cells);
          if (filtered.length !== changedCells.length) {
            cells = new Set(syncInfo.uris);
            for (const cell of changedCells) {
              cells.delete(cell.document.uri.toString());
            }
            for (const cell of filtered) {
              cells.add(cell.document.uri.toString());
            }
          }
        }
        if (event.contentChanges !== void 0 && event.contentChanges.length > 0) {
          if (cells === void 0) {
            cells = new Set(syncInfo.uris);
          }
          for (const item of event.contentChanges) {
            for (const cell of item.removedCells) {
              cells.delete(cell.document.uri.toString());
            }
            const filtered = this.filterCells(notebookDocument, new Array(...item.addedCells), selector.cells);
            for (const cell of filtered) {
              cells.add(cell.document.uri.toString());
            }
          }
        }
        if (cells === void 0) {
          return syncInfo.cells;
        }
        const result = [];
        const current = notebookDocument.getCells();
        for (const cell of current) {
          if (cells.has(cell.document.uri.toString())) {
            result.push(cell);
          }
        }
        return result;
      }
      getMatchingCellsFromSyncInfo(notebook) {
        const syncInfo = this.getSyncInfo(notebook);
        return syncInfo !== void 0 ? syncInfo.cells : void 0;
      }
      getMatchingCellsConsideringSyncInfo(notebook) {
        const syncInfo = this.getSyncInfo(notebook);
        return syncInfo !== void 0 ? syncInfo.cells : this.getMatchingCells(notebook);
      }
      mergeCells(notebookDocument, syncInfo, cells) {
        const result = [];
        const merged = new Set(syncInfo.uris);
        for (const cell of cells) {
          merged.add(cell.document.uri.toString());
        }
        for (const cell of notebookDocument.getCells()) {
          if (merged.has(cell.document.uri.toString())) {
            result.push(cell);
          }
        }
        return result;
      }
      cellMatches(notebookDocument, cell) {
        const cells = this.getMatchingCells(notebookDocument, [cell]);
        return cells !== void 0 && cells[0] === cell;
      }
      filterCells(notebookDocument, cells, cellSelector) {
        const filtered = cellSelector !== void 0 ? cells.filter((cell) => {
          const cellLanguage = cell.document.languageId;
          return cellSelector.some(((filter) => filter.language === "*" || cellLanguage === filter.language));
        }) : cells;
        return typeof this.client.clientOptions.notebookDocumentOptions?.filterCells === "function" ? this.client.clientOptions.notebookDocumentOptions.filterCells(notebookDocument, filtered) : filtered;
      }
      getSyncInfo(notebook) {
        return this.notebookSyncInfo.get(notebook.uri.toString());
      }
    };
    var NotebookDocumentSyncFeature = class _NotebookDocumentSyncFeature {
      static CellScheme = "vscode-notebook-cell";
      client;
      registrations;
      dedicatedChannel;
      _onChangeNotificationSent;
      _onOpenNotificationSent;
      _onCloseNotificationSent;
      _onSaveNotificationSent;
      constructor(client) {
        this.client = client;
        this.registrations = /* @__PURE__ */ new Map();
        this.registrationType = proto.NotebookDocumentSyncRegistrationType.type;
        this._onChangeNotificationSent = new vscode2.EventEmitter();
        this._onOpenNotificationSent = new vscode2.EventEmitter();
        this._onCloseNotificationSent = new vscode2.EventEmitter();
        this._onSaveNotificationSent = new vscode2.EventEmitter();
        vscode2.workspace.onDidOpenTextDocument((textDocument) => {
          if (textDocument.uri.scheme !== _NotebookDocumentSyncFeature.CellScheme) {
            return;
          }
          const [notebookDocument, notebookCell] = this.findNotebookDocumentAndCell(textDocument);
          if (notebookDocument === void 0 || notebookCell === void 0) {
            return;
          }
          for (const provider of this.registrations.values()) {
            if (provider instanceof NotebookDocumentSyncFeatureProvider) {
              provider.didOpenNotebookCellTextDocument(notebookDocument, notebookCell);
            }
          }
        });
        vscode2.workspace.onDidChangeTextDocument((event) => {
          if (event.contentChanges.length === 0) {
            return;
          }
          const textDocument = event.document;
          if (textDocument.uri.scheme !== _NotebookDocumentSyncFeature.CellScheme) {
            return;
          }
          const [notebookDocument, cell] = this.findNotebookDocumentAndCell(textDocument);
          if (notebookDocument === void 0 || cell === void 0) {
            return;
          }
          for (const provider of this.registrations.values()) {
            if (provider instanceof NotebookDocumentSyncFeatureProvider) {
              provider.didChangeNotebookCellTextDocument(notebookDocument, cell, event);
            }
          }
        });
        vscode2.workspace.onDidCloseTextDocument((textDocument) => {
          if (textDocument.uri.scheme !== _NotebookDocumentSyncFeature.CellScheme) {
            return;
          }
          const [notebookDocument, notebookCell] = this.findNotebookDocumentAndCell(textDocument);
          if (notebookDocument === void 0 || notebookCell === void 0) {
            return;
          }
          for (const provider of this.registrations.values()) {
            if (provider instanceof NotebookDocumentSyncFeatureProvider) {
              provider.didCloseNotebookCellTextDocument(notebookDocument, notebookCell);
            }
          }
        });
      }
      getState() {
        if (this.registrations.size === 0) {
          return { kind: "document", id: this.registrationType.method, registrations: false, matches: false };
        }
        for (const provider of this.registrations.values()) {
          const state = provider.getState();
          if (state.kind === "document" && state.registrations === true && state.matches === true) {
            return { kind: "document", id: this.registrationType.method, registrations: true, matches: true };
          }
        }
        return { kind: "document", id: this.registrationType.method, registrations: true, matches: false };
      }
      registrationType;
      get onOpenNotificationSent() {
        return this._onOpenNotificationSent.event;
      }
      get onChangeNotificationSent() {
        return this._onChangeNotificationSent.event;
      }
      get onCloseNotificationSent() {
        return this._onCloseNotificationSent.event;
      }
      get onSaveNotificationSent() {
        return this._onSaveNotificationSent.event;
      }
      fillClientCapabilities(capabilities) {
        const synchronization = ensure(ensure(capabilities, "notebookDocument"), "synchronization");
        synchronization.dynamicRegistration = true;
        synchronization.executionSummarySupport = true;
      }
      preInitialize(capabilities) {
        const options = capabilities.notebookDocumentSync;
        if (options === void 0) {
          return;
        }
        this.dedicatedChannel = this.client.protocol2CodeConverter.asDocumentSelector($NotebookDocumentSyncOptions.asDocumentSelector(options));
      }
      initialize(capabilities) {
        const options = capabilities.notebookDocumentSync;
        if (options === void 0) {
          return;
        }
        const id = options.id ?? UUID.generateUuid();
        this.register({ id, registerOptions: options });
      }
      register(data) {
        const provider = new NotebookDocumentSyncFeatureProvider(this.client, data.registerOptions, this._onChangeNotificationSent, this._onOpenNotificationSent, this._onCloseNotificationSent, this._onSaveNotificationSent);
        this.registrations.set(data.id, provider);
      }
      unregister(id) {
        const provider = this.registrations.get(id);
        if (provider !== void 0) {
          this.registrations.delete(id);
          provider.dispose();
        }
      }
      clear() {
        for (const provider of this.registrations.values()) {
          provider.dispose();
        }
        this.registrations.clear();
        this._onChangeNotificationSent.dispose();
        this._onChangeNotificationSent = new vscode2.EventEmitter();
        this._onOpenNotificationSent.dispose();
        this._onOpenNotificationSent = new vscode2.EventEmitter();
        this._onCloseNotificationSent.dispose();
        this._onCloseNotificationSent = new vscode2.EventEmitter();
        this._onSaveNotificationSent.dispose();
        this._onSaveNotificationSent = new vscode2.EventEmitter();
      }
      handles(textDocument) {
        if (textDocument.uri.scheme !== _NotebookDocumentSyncFeature.CellScheme) {
          return false;
        }
        if (this.dedicatedChannel !== void 0 && vscode2.languages.match(this.dedicatedChannel, textDocument) > 0) {
          return true;
        }
        for (const provider of this.registrations.values()) {
          if (provider.handles(textDocument)) {
            return true;
          }
        }
        return false;
      }
      getProvider(notebookCell) {
        for (const provider of this.registrations.values()) {
          if (provider.handles(notebookCell.document)) {
            return provider;
          }
        }
        return void 0;
      }
      findNotebookDocumentAndCell(textDocument) {
        const uri = textDocument.uri.toString();
        for (const notebookDocument of vscode2.workspace.notebookDocuments) {
          for (const cell of notebookDocument.getCells()) {
            if (cell.document.uri.toString() === uri) {
              return [notebookDocument, cell];
            }
          }
        }
        return [void 0, void 0];
      }
    };
    exports2.NotebookDocumentSyncFeature = NotebookDocumentSyncFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/configuration.js
var require_configuration = __commonJS({
  "node_modules/vscode-languageclient/lib/common/configuration.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SyncConfigurationFeature = exports2.ConfigurationFeature = void 0;
    exports2.toJSONObject = toJSONObject;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var Is2 = __importStar(require_is());
    var UUID = __importStar(require_uuid());
    var features_1 = require_features();
    var ConfigurationFeature = class {
      _client;
      constructor(client) {
        this._client = client;
      }
      getState() {
        return { kind: "static" };
      }
      fillClientCapabilities(capabilities) {
        capabilities.workspace = capabilities.workspace || {};
        capabilities.workspace.configuration = true;
      }
      initialize() {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.ConfigurationRequest.type, (params, token) => {
          const configuration = (params2) => {
            const result = [];
            for (const item of params2.items) {
              const resource = item.scopeUri !== void 0 && item.scopeUri !== null ? this._client.protocol2CodeConverter.asUri(item.scopeUri) : void 0;
              result.push(this.getConfiguration(resource, item.section !== null ? item.section : void 0));
            }
            return result;
          };
          const middleware = client.middleware.workspace;
          return middleware && middleware.configuration ? middleware.configuration(params, token, configuration) : configuration(params, token);
        });
      }
      getConfiguration(resource, section) {
        let result = null;
        if (section) {
          const index = section.lastIndexOf(".");
          if (index === -1) {
            result = toJSONObject(vscode_1.workspace.getConfiguration(void 0, resource).get(section));
          } else {
            const config = vscode_1.workspace.getConfiguration(section.substr(0, index), resource);
            if (config) {
              result = toJSONObject(config.get(section.substr(index + 1)));
            }
          }
        } else {
          const config = vscode_1.workspace.getConfiguration(void 0, resource);
          result = {};
          for (const key of Object.keys(config)) {
            if (config.has(key)) {
              result[key] = toJSONObject(config.get(key));
            }
          }
        }
        if (result === void 0) {
          result = null;
        }
        return result;
      }
      clear() {
      }
    };
    exports2.ConfigurationFeature = ConfigurationFeature;
    function toJSONObject(obj) {
      if (obj) {
        if (Array.isArray(obj)) {
          return obj.map(toJSONObject);
        } else if (typeof obj === "object") {
          const res = /* @__PURE__ */ Object.create(null);
          for (const key in obj) {
            if (Object.prototype.hasOwnProperty.call(obj, key)) {
              res[key] = toJSONObject(obj[key]);
            }
          }
          return res;
        }
      }
      return obj;
    }
    var SyncConfigurationFeature = class {
      _client;
      isCleared;
      _listeners;
      constructor(_client) {
        this._client = _client;
        this.isCleared = false;
        this._listeners = /* @__PURE__ */ new Map();
      }
      getState() {
        return { kind: "workspace", id: this.registrationType.method, registrations: this._listeners.size > 0 };
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidChangeConfigurationNotification.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "didChangeConfiguration").dynamicRegistration = true;
      }
      initialize() {
        this.isCleared = false;
        const section = this._client.clientOptions.synchronize?.configurationSection;
        if (section !== void 0) {
          this.register({
            id: UUID.generateUuid(),
            registerOptions: {
              section
            }
          });
        }
      }
      register(data) {
        const disposable = vscode_1.workspace.onDidChangeConfiguration((event) => {
          this.onDidChangeConfiguration(data.registerOptions.section, event);
        });
        this._listeners.set(data.id, disposable);
        if (data.registerOptions.section !== void 0) {
          this.onDidChangeConfiguration(data.registerOptions.section, void 0);
        }
      }
      unregister(id) {
        const disposable = this._listeners.get(id);
        if (disposable) {
          this._listeners.delete(id);
          disposable.dispose();
        }
      }
      clear() {
        for (const disposable of this._listeners.values()) {
          disposable.dispose();
        }
        this._listeners.clear();
        this.isCleared = true;
      }
      onDidChangeConfiguration(configurationSection, event) {
        if (this.isCleared) {
          return;
        }
        let sections;
        if (Is2.string(configurationSection)) {
          sections = [configurationSection];
        } else {
          sections = configurationSection;
        }
        if (sections !== void 0 && event !== void 0) {
          const affected = sections.some((section) => event.affectsConfiguration(section));
          if (!affected) {
            return;
          }
        }
        const didChangeConfiguration = async (sections2) => {
          if (sections2 === void 0) {
            return this._client.sendNotification(vscode_languageserver_protocol_1.DidChangeConfigurationNotification.type, { settings: null });
          } else {
            return this._client.sendNotification(vscode_languageserver_protocol_1.DidChangeConfigurationNotification.type, { settings: this.extractSettingsInformation(sections2) });
          }
        };
        const middleware = this._client.middleware.workspace?.didChangeConfiguration;
        (middleware ? middleware(sections, didChangeConfiguration) : didChangeConfiguration(sections)).catch((error) => {
          this._client.error(`Sending notification ${vscode_languageserver_protocol_1.DidChangeConfigurationNotification.type.method} failed`, error);
        });
      }
      extractSettingsInformation(keys) {
        function ensurePath(config, path) {
          let current = config;
          for (let i = 0; i < path.length - 1; i++) {
            let obj = current[path[i]];
            if (!obj) {
              obj = /* @__PURE__ */ Object.create(null);
              current[path[i]] = obj;
            }
            current = obj;
          }
          return current;
        }
        const resource = this._client.clientOptions.workspaceFolder ? this._client.clientOptions.workspaceFolder.uri : void 0;
        const result = /* @__PURE__ */ Object.create(null);
        for (let i = 0; i < keys.length; i++) {
          const key = keys[i];
          const index = key.indexOf(".");
          let config = null;
          if (index >= 0) {
            config = vscode_1.workspace.getConfiguration(key.substr(0, index), resource).get(key.substr(index + 1));
          } else {
            config = vscode_1.workspace.getConfiguration(void 0, resource).get(key);
          }
          if (config) {
            const path = keys[i].split(".");
            ensurePath(result, path)[path[path.length - 1]] = toJSONObject(config);
          }
        }
        return result;
      }
    };
    exports2.SyncConfigurationFeature = SyncConfigurationFeature;
  }
});

// node_modules/vscode-languageserver-textdocument/lib/esm/main.js
var main_exports2 = {};
__export(main_exports2, {
  TextDocument: () => TextDocument2
});
function mergeSort(data, compare) {
  if (data.length <= 1) {
    return data;
  }
  const p = data.length / 2 | 0;
  const left = data.slice(0, p);
  const right = data.slice(p);
  mergeSort(left, compare);
  mergeSort(right, compare);
  let leftIdx = 0;
  let rightIdx = 0;
  let i = 0;
  while (leftIdx < left.length && rightIdx < right.length) {
    const ret = compare(left[leftIdx], right[rightIdx]);
    if (ret <= 0) {
      data[i++] = left[leftIdx++];
    } else {
      data[i++] = right[rightIdx++];
    }
  }
  while (leftIdx < left.length) {
    data[i++] = left[leftIdx++];
  }
  while (rightIdx < right.length) {
    data[i++] = right[rightIdx++];
  }
  return data;
}
function computeLineOffsets(text, isAtLineStart, textOffset = 0) {
  const result = isAtLineStart ? [textOffset] : [];
  for (let i = 0; i < text.length; i++) {
    const ch = text.charCodeAt(i);
    if (isEOL(ch)) {
      if (ch === 13 && i + 1 < text.length && text.charCodeAt(i + 1) === 10) {
        i++;
      }
      result.push(textOffset + i + 1);
    }
  }
  return result;
}
function isEOL(char) {
  return char === 13 || char === 10;
}
function getWellformedRange(range) {
  const start = range.start;
  const end = range.end;
  if (start.line > end.line || start.line === end.line && start.character > end.character) {
    return { start: end, end: start };
  }
  return range;
}
function getWellformedEdit(textEdit) {
  const range = getWellformedRange(textEdit.range);
  if (range !== textEdit.range) {
    return { newText: textEdit.newText, range };
  }
  return textEdit;
}
var FullTextDocument2, TextDocument2;
var init_main2 = __esm({
  "node_modules/vscode-languageserver-textdocument/lib/esm/main.js"() {
    "use strict";
    FullTextDocument2 = class _FullTextDocument {
      constructor(uri, languageId, version, content) {
        this._uri = uri;
        this._languageId = languageId;
        this._version = version;
        this._content = content;
        this._lineOffsets = void 0;
      }
      get uri() {
        return this._uri;
      }
      get languageId() {
        return this._languageId;
      }
      get version() {
        return this._version;
      }
      getText(range) {
        if (range) {
          const start = this.offsetAt(range.start);
          const end = this.offsetAt(range.end);
          return this._content.substring(start, end);
        }
        return this._content;
      }
      update(changes, version) {
        for (const change of changes) {
          if (_FullTextDocument.isIncremental(change)) {
            const range = getWellformedRange(change.range);
            const startOffset = this.offsetAt(range.start);
            const endOffset = this.offsetAt(range.end);
            this._content = this._content.substring(0, startOffset) + change.text + this._content.substring(endOffset, this._content.length);
            const startLine = Math.max(range.start.line, 0);
            const endLine = Math.max(range.end.line, 0);
            let lineOffsets = this._lineOffsets;
            const addedLineOffsets = computeLineOffsets(change.text, false, startOffset);
            if (endLine - startLine === addedLineOffsets.length) {
              for (let i = 0, len = addedLineOffsets.length; i < len; i++) {
                lineOffsets[i + startLine + 1] = addedLineOffsets[i];
              }
            } else {
              if (addedLineOffsets.length < 1e4) {
                lineOffsets.splice(startLine + 1, endLine - startLine, ...addedLineOffsets);
              } else {
                this._lineOffsets = lineOffsets = lineOffsets.slice(0, startLine + 1).concat(addedLineOffsets, lineOffsets.slice(endLine + 1));
              }
            }
            const diff = change.text.length - (endOffset - startOffset);
            if (diff !== 0) {
              for (let i = startLine + 1 + addedLineOffsets.length, len = lineOffsets.length; i < len; i++) {
                lineOffsets[i] = lineOffsets[i] + diff;
              }
            }
          } else if (_FullTextDocument.isFull(change)) {
            this._content = change.text;
            this._lineOffsets = void 0;
          } else {
            throw new Error("Unknown change event received");
          }
        }
        this._version = version;
      }
      getLineOffsets() {
        if (this._lineOffsets === void 0) {
          this._lineOffsets = computeLineOffsets(this._content, true);
        }
        return this._lineOffsets;
      }
      positionAt(offset) {
        offset = Math.max(Math.min(offset, this._content.length), 0);
        const lineOffsets = this.getLineOffsets();
        let low = 0, high = lineOffsets.length;
        if (high === 0) {
          return { line: 0, character: offset };
        }
        while (low < high) {
          const mid = Math.floor((low + high) / 2);
          if (lineOffsets[mid] > offset) {
            high = mid;
          } else {
            low = mid + 1;
          }
        }
        const line = low - 1;
        offset = this.ensureBeforeEOL(offset, lineOffsets[line]);
        return { line, character: offset - lineOffsets[line] };
      }
      offsetAt(position) {
        const lineOffsets = this.getLineOffsets();
        if (position.line >= lineOffsets.length) {
          return this._content.length;
        } else if (position.line < 0) {
          return 0;
        }
        const lineOffset = lineOffsets[position.line];
        if (position.character <= 0) {
          return lineOffset;
        }
        const nextLineOffset = position.line + 1 < lineOffsets.length ? lineOffsets[position.line + 1] : this._content.length;
        const offset = Math.min(lineOffset + position.character, nextLineOffset);
        return this.ensureBeforeEOL(offset, lineOffset);
      }
      getLineRange(line) {
        const lineOffsets = this.getLineOffsets();
        if (line >= lineOffsets.length) {
          const lastLine = lineOffsets.length - 1;
          return { start: { line: lastLine, character: 0 }, end: { line: lastLine, character: this._content.length - lineOffsets[lastLine] } };
        } else if (line < 0) {
          return { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } };
        }
        const startOffset = lineOffsets[line];
        const nextLineOffset = line + 1 < lineOffsets.length ? lineOffsets[line + 1] : this._content.length;
        const endOffset = this.ensureBeforeEOL(nextLineOffset, startOffset);
        return { start: { line, character: 0 }, end: { line, character: endOffset - startOffset } };
      }
      getEOLCharacters(line) {
        const lineOffsets = this.getLineOffsets();
        if (line >= lineOffsets.length) {
          return "";
        } else if (line < 0) {
          return "";
        }
        const nextLineOffset = line + 1 < lineOffsets.length ? lineOffsets[line + 1] : this._content.length;
        const eolOffset = this.ensureBeforeEOL(nextLineOffset, lineOffsets[line]);
        return this._content.substring(eolOffset, nextLineOffset);
      }
      ensureBeforeEOL(offset, lineOffset) {
        while (offset > lineOffset && isEOL(this._content.charCodeAt(offset - 1))) {
          offset--;
        }
        return offset;
      }
      get lineCount() {
        return this.getLineOffsets().length;
      }
      static isIncremental(event) {
        const candidate = event;
        return candidate !== void 0 && candidate !== null && typeof candidate.text === "string" && candidate.range !== void 0 && (candidate.rangeLength === void 0 || typeof candidate.rangeLength === "number");
      }
      static isFull(event) {
        const candidate = event;
        return candidate !== void 0 && candidate !== null && typeof candidate.text === "string" && candidate.range === void 0 && candidate.rangeLength === void 0;
      }
    };
    (function(TextDocument3) {
      function create(uri, languageId, version, content) {
        return new FullTextDocument2(uri, languageId, version, content);
      }
      TextDocument3.create = create;
      function update(document, changes, version) {
        if (document instanceof FullTextDocument2) {
          document.update(changes, version);
          return document;
        } else {
          throw new Error("TextDocument.update: document must be created by TextDocument.create");
        }
      }
      TextDocument3.update = update;
      function applyEdits(document, edits) {
        const text = document.getText();
        const sortedEdits = mergeSort(edits.map(getWellformedEdit), (a, b) => {
          const diff = a.range.start.line - b.range.start.line;
          if (diff === 0) {
            return a.range.start.character - b.range.start.character;
          }
          return diff;
        });
        let lastModifiedOffset = 0;
        const spans = [];
        for (const e of sortedEdits) {
          const startOffset = document.offsetAt(e.range.start);
          if (startOffset < lastModifiedOffset) {
            throw new Error("Overlapping edit");
          } else if (startOffset > lastModifiedOffset) {
            spans.push(text.substring(lastModifiedOffset, startOffset));
          }
          if (e.newText.length) {
            spans.push(e.newText);
          }
          lastModifiedOffset = document.offsetAt(e.range.end);
        }
        spans.push(text.substr(lastModifiedOffset));
        return spans.join("");
      }
      TextDocument3.applyEdits = applyEdits;
    })(TextDocument2 || (TextDocument2 = {}));
  }
});

// node_modules/vscode-languageclient/lib/common/textSynchronization.js
var require_textSynchronization = __commonJS({
  "node_modules/vscode-languageclient/lib/common/textSynchronization.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DidSaveTextDocumentFeature = exports2.WillSaveWaitUntilFeature = exports2.WillSaveFeature = exports2.DidChangeTextDocumentFeature = exports2.DidCloseTextDocumentFeature = exports2.DidOpenTextDocumentFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var vscode_languageserver_textdocument_1 = (init_main2(), __toCommonJS(main_exports2));
    var DidOpenTextDocumentFeature = class extends features_1.TextDocumentEventFeature {
      _syncedDocuments;
      _pendingOpenNotifications;
      _delayOpen;
      _pendingOpenListeners;
      constructor(client, syncedDocuments) {
        super(client, vscode_1.workspace.onDidOpenTextDocument, vscode_languageserver_protocol_1.DidOpenTextDocumentNotification.type, () => client.middleware.didOpen, (textDocument) => client.code2ProtocolConverter.asOpenTextDocumentParams(textDocument), (data) => data, features_1.TextDocumentEventFeature.textDocumentFilter);
        this._syncedDocuments = syncedDocuments;
        this._pendingOpenNotifications = /* @__PURE__ */ new Map();
        this._delayOpen = client.clientOptions.textSynchronization?.delayOpenNotifications ?? false;
      }
      async callback(document) {
        if (!this._delayOpen) {
          return super.callback(document);
        } else {
          if (!this.matches(document)) {
            return;
          }
          const visibleDocuments = this._client.visibleDocuments;
          if (visibleDocuments.isVisible(document)) {
            return super.callback(document);
          } else {
            const snapshot = new TextDocumentSnapshot(document);
            this._pendingOpenNotifications.set(snapshot.uri.toString(), snapshot);
          }
        }
      }
      get openDocuments() {
        return this._syncedDocuments.values();
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.openClose) {
          this.register({ id: UUID.generateUuid(), registerOptions: { documentSelector } });
        }
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidOpenTextDocumentNotification.type;
      }
      register(data) {
        super.register(data);
        if (!data.registerOptions.documentSelector) {
          return;
        }
        const documentSelector = this._client.protocol2CodeConverter.asDocumentSelector(data.registerOptions.documentSelector);
        vscode_1.workspace.textDocuments.forEach((textDocument) => {
          const uri = textDocument.uri.toString();
          if (this._syncedDocuments.has(uri)) {
            return;
          }
          if (vscode_1.languages.match(documentSelector, textDocument) > 0 && !this._client.hasDedicatedTextSynchronizationFeature(textDocument)) {
            const visibleDocuments = this._client.visibleDocuments;
            if (visibleDocuments.isVisible(textDocument)) {
              const middleware = this._client.middleware;
              const didOpen = (textDocument2) => {
                return this._client.sendNotification(this._type, this._createParams(textDocument2));
              };
              (middleware.didOpen ? middleware.didOpen(textDocument, didOpen) : didOpen(textDocument)).catch((error) => {
                this._client.error(`Sending document notification ${this._type.method} failed`, error);
              });
              this._syncedDocuments.set(uri, textDocument);
            } else {
              this._pendingOpenNotifications.set(uri, textDocument);
            }
          }
        });
        if (this._delayOpen && this._pendingOpenListeners === void 0) {
          this._pendingOpenListeners = [];
          const visibleDocuments = this._client.visibleDocuments;
          this._pendingOpenListeners.push(visibleDocuments.onClose((closed) => {
            for (const uri of closed) {
              this._pendingOpenNotifications.delete(uri.toString());
            }
          }));
          this._pendingOpenListeners.push(visibleDocuments.onOpen((opened) => {
            for (const uri of opened) {
              const document = this._pendingOpenNotifications.get(uri.toString());
              if (document !== void 0) {
                super.callback(document).catch(((error) => {
                  this._client.error(`Sending document notification ${this._type.method} failed`, error);
                }));
                this._pendingOpenNotifications.delete(uri.toString());
              }
            }
          }));
          this._pendingOpenListeners.push(vscode_1.workspace.onDidCloseTextDocument((document) => {
            this._pendingOpenNotifications.delete(document.uri.toString());
          }));
        }
      }
      /**
       * Sends any pending open notifications unless they are for the document
       * being closed.
       *
       * @param closingDocument The document being closed.
       * @returns Whether a pending open notification was dropped because it was
       *          for the closing document.
       */
      async sendPendingOpenNotifications(closingDocument) {
        const notifications = Array.from(this._pendingOpenNotifications.values());
        this._pendingOpenNotifications.clear();
        let didDropOpenNotification = false;
        for (const notification of notifications) {
          if (closingDocument !== void 0 && notification.uri.toString() === closingDocument) {
            didDropOpenNotification = true;
            continue;
          }
          await super.callback(notification);
        }
        return didDropOpenNotification;
      }
      getTextDocument(data) {
        return data;
      }
      notificationSent(textDocument, type, params) {
        this._syncedDocuments.set(textDocument.uri.toString(), textDocument);
        super.notificationSent(textDocument, type, params);
      }
      clear() {
        this._pendingOpenNotifications.clear();
        if (this._pendingOpenListeners !== void 0) {
          for (const listener of this._pendingOpenListeners) {
            listener.dispose();
          }
          this._pendingOpenListeners = void 0;
        }
        super.clear();
      }
    };
    exports2.DidOpenTextDocumentFeature = DidOpenTextDocumentFeature;
    var DidCloseTextDocumentFeature = class extends features_1.TextDocumentEventFeature {
      _syncedDocuments;
      _pendingTextDocumentChanges;
      constructor(client, syncedDocuments, pendingTextDocumentChanges) {
        super(client, vscode_1.workspace.onDidCloseTextDocument, vscode_languageserver_protocol_1.DidCloseTextDocumentNotification.type, () => client.middleware.didClose, (textDocument) => client.code2ProtocolConverter.asCloseTextDocumentParams(textDocument), (data) => data, features_1.TextDocumentEventFeature.textDocumentFilter);
        this._syncedDocuments = syncedDocuments;
        this._pendingTextDocumentChanges = pendingTextDocumentChanges;
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidCloseTextDocumentNotification.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.openClose) {
          this.register({ id: UUID.generateUuid(), registerOptions: { documentSelector } });
        }
      }
      async callback(data) {
        await super.callback(data);
        this._pendingTextDocumentChanges.delete(data.uri.toString());
      }
      getTextDocument(data) {
        return data;
      }
      notificationSent(textDocument, type, params) {
        this._syncedDocuments.delete(textDocument.uri.toString());
        super.notificationSent(textDocument, type, params);
      }
      unregister(id) {
        const selector = this._selectors.get(id);
        if (selector === void 0) {
          return;
        }
        super.unregister(id);
        const selectors = this._selectors.values();
        this._syncedDocuments.forEach((textDocument) => {
          if (vscode_1.languages.match(selector, textDocument) > 0 && !this._selectorFilter(selectors, textDocument) && !this._client.hasDedicatedTextSynchronizationFeature(textDocument)) {
            const middleware = this._client.middleware;
            const didClose = (textDocument2) => {
              return this._client.sendNotification(this._type, this._createParams(textDocument2));
            };
            this._syncedDocuments.delete(textDocument.uri.toString());
            (middleware.didClose ? middleware.didClose(textDocument, didClose) : didClose(textDocument)).catch((error) => {
              this._client.error(`Sending document notification ${this._type.method} failed`, error);
            });
          }
        });
      }
    };
    exports2.DidCloseTextDocumentFeature = DidCloseTextDocumentFeature;
    var DidChangeTextDocumentFeature = class extends features_1.DynamicDocumentFeature {
      _listener;
      _changeData;
      _onAboutToSendNotification;
      _onNotificationSent;
      _onPendingChangeAdded;
      _pendingTextDocumentChanges;
      _syncKind;
      constructor(client, pendingTextDocumentChanges) {
        super(client);
        this._changeData = /* @__PURE__ */ new Map();
        this._onAboutToSendNotification = new vscode_1.EventEmitter();
        this._onNotificationSent = new vscode_1.EventEmitter();
        this._onPendingChangeAdded = new vscode_1.EventEmitter();
        this._pendingTextDocumentChanges = pendingTextDocumentChanges;
        this._syncKind = vscode_languageserver_protocol_1.TextDocumentSyncKind.None;
      }
      get onAboutToSendNotification() {
        return this._onAboutToSendNotification.event;
      }
      get onNotificationSent() {
        return this._onNotificationSent.event;
      }
      get onPendingChangeAdded() {
        return this._onPendingChangeAdded.event;
      }
      get syncKind() {
        return this._syncKind;
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.change !== void 0 && textDocumentSyncOptions.change !== vscode_languageserver_protocol_1.TextDocumentSyncKind.None) {
          this.register({
            id: UUID.generateUuid(),
            registerOptions: Object.assign({}, { documentSelector }, { syncKind: textDocumentSyncOptions.change })
          });
        }
      }
      register(data) {
        if (!data.registerOptions.documentSelector) {
          return;
        }
        if (!this._listener) {
          this._listener = vscode_1.workspace.onDidChangeTextDocument(this.callback, this);
        }
        this._changeData.set(data.id, {
          syncKind: data.registerOptions.syncKind,
          documentSelector: this._client.protocol2CodeConverter.asDocumentSelector(data.registerOptions.documentSelector)
        });
        this.updateSyncKind(data.registerOptions.syncKind);
      }
      *getDocumentSelectors() {
        for (const data of this._changeData.values()) {
          yield data.documentSelector;
        }
      }
      async callback(event) {
        if (event.contentChanges.length === 0) {
          return;
        }
        const uri = event.document.uri;
        const version = event.document.version;
        const promises = [];
        for (const changeData of this._changeData.values()) {
          if (vscode_1.languages.match(changeData.documentSelector, event.document) > 0 && !this._client.hasDedicatedTextSynchronizationFeature(event.document)) {
            const middleware = this._client.middleware;
            if (changeData.syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Incremental) {
              const didChange = async (event2) => {
                const params = this._client.code2ProtocolConverter.asChangeTextDocumentParams(event2, uri, version);
                this.aboutToSendNotification(event2.document, vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
                await this._client.sendNotification(vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
                this.notificationSent(event2.document, vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
              };
              promises.push(middleware.didChange ? middleware.didChange(event, (event2) => didChange(event2)) : didChange(event));
            } else if (changeData.syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Full) {
              const didChange = async (event2) => {
                const eventUri = event2.document.uri.toString();
                this._pendingTextDocumentChanges.set(eventUri, event2.document);
                this._onPendingChangeAdded.fire();
              };
              promises.push(middleware.didChange ? middleware.didChange(event, (event2) => didChange(event2)) : didChange(event));
            }
          }
        }
        return Promise.all(promises).then(void 0, (error) => {
          this._client.error(`Sending document notification ${vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type.method} failed`, error);
          throw error;
        });
      }
      aboutToSendNotification(textDocument, type, params) {
        this._onAboutToSendNotification.fire({ textDocument, type, params });
      }
      notificationSent(textDocument, type, params) {
        this._onNotificationSent.fire({ textDocument, type, params });
      }
      unregister(id) {
        this._changeData.delete(id);
        if (this._changeData.size === 0) {
          if (this._listener) {
            this._listener.dispose();
            this._listener = void 0;
          }
          this._syncKind = vscode_languageserver_protocol_1.TextDocumentSyncKind.None;
        } else {
          this._syncKind = vscode_languageserver_protocol_1.TextDocumentSyncKind.None;
          for (const changeData of this._changeData.values()) {
            this.updateSyncKind(changeData.syncKind);
            if (this._syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Full) {
              break;
            }
          }
        }
      }
      clear() {
        this._pendingTextDocumentChanges.clear();
        this._changeData.clear();
        this._syncKind = vscode_languageserver_protocol_1.TextDocumentSyncKind.None;
        if (this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      getPendingDocumentChanges(excludes) {
        if (this._pendingTextDocumentChanges.size === 0) {
          return [];
        }
        let result;
        if (excludes.size === 0) {
          result = Array.from(this._pendingTextDocumentChanges.values());
          this._pendingTextDocumentChanges.clear();
        } else {
          result = [];
          for (const entry of this._pendingTextDocumentChanges) {
            if (!excludes.has(entry[0])) {
              result.push(entry[1]);
              this._pendingTextDocumentChanges.delete(entry[0]);
            }
          }
        }
        return result;
      }
      getProvider(document) {
        for (const changeData of this._changeData.values()) {
          if (vscode_1.languages.match(changeData.documentSelector, document) > 0) {
            return {
              send: (event) => {
                return this.callback(event);
              }
            };
          }
        }
        return void 0;
      }
      updateSyncKind(syncKind) {
        if (this._syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Full) {
          return;
        }
        switch (syncKind) {
          case vscode_languageserver_protocol_1.TextDocumentSyncKind.Full:
            this._syncKind = syncKind;
            break;
          case vscode_languageserver_protocol_1.TextDocumentSyncKind.Incremental:
            if (this._syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.None) {
              this._syncKind = vscode_languageserver_protocol_1.TextDocumentSyncKind.Incremental;
            }
            break;
        }
      }
    };
    exports2.DidChangeTextDocumentFeature = DidChangeTextDocumentFeature;
    var WillSaveFeature = class extends features_1.TextDocumentEventFeature {
      constructor(client) {
        super(client, vscode_1.workspace.onWillSaveTextDocument, vscode_languageserver_protocol_1.WillSaveTextDocumentNotification.type, () => client.middleware.willSave, (willSaveEvent) => client.code2ProtocolConverter.asWillSaveTextDocumentParams(willSaveEvent), (event) => event.document, (selectors, willSaveEvent) => features_1.TextDocumentEventFeature.textDocumentFilter(selectors, willSaveEvent.document));
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.WillSaveTextDocumentNotification.type;
      }
      fillClientCapabilities(capabilities) {
        const value = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization");
        value.willSave = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.willSave) {
          this.register({
            id: UUID.generateUuid(),
            registerOptions: { documentSelector }
          });
        }
      }
      getTextDocument(data) {
        return data.document;
      }
    };
    exports2.WillSaveFeature = WillSaveFeature;
    var WillSaveWaitUntilFeature = class extends features_1.DynamicDocumentFeature {
      _listener;
      _selectors;
      constructor(client) {
        super(client);
        this._selectors = /* @__PURE__ */ new Map();
      }
      getDocumentSelectors() {
        return this._selectors.values();
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.WillSaveTextDocumentWaitUntilRequest.type;
      }
      fillClientCapabilities(capabilities) {
        const value = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization");
        value.willSaveWaitUntil = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.willSaveWaitUntil) {
          this.register({
            id: UUID.generateUuid(),
            registerOptions: { documentSelector }
          });
        }
      }
      register(data) {
        if (!data.registerOptions.documentSelector) {
          return;
        }
        if (!this._listener) {
          this._listener = vscode_1.workspace.onWillSaveTextDocument(this.callback, this);
        }
        this._selectors.set(data.id, this._client.protocol2CodeConverter.asDocumentSelector(data.registerOptions.documentSelector));
      }
      callback(event) {
        if (features_1.TextDocumentEventFeature.textDocumentFilter(this._selectors.values(), event.document) && !this._client.hasDedicatedTextSynchronizationFeature(event.document)) {
          const middleware = this._client.middleware;
          const willSaveWaitUntil = (event2) => {
            return this._client.sendRequest(vscode_languageserver_protocol_1.WillSaveTextDocumentWaitUntilRequest.type, this._client.code2ProtocolConverter.asWillSaveTextDocumentParams(event2)).then(async (edits) => {
              const vEdits = await this._client.protocol2CodeConverter.asTextEdits(edits);
              return vEdits === void 0 ? [] : vEdits;
            });
          };
          event.waitUntil(middleware.willSaveWaitUntil ? middleware.willSaveWaitUntil(event, willSaveWaitUntil) : willSaveWaitUntil(event));
        }
      }
      unregister(id) {
        this._selectors.delete(id);
        if (this._selectors.size === 0 && this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      clear() {
        this._selectors.clear();
        if (this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
    };
    exports2.WillSaveWaitUntilFeature = WillSaveWaitUntilFeature;
    var DidSaveTextDocumentFeature = class extends features_1.TextDocumentEventFeature {
      _includeText;
      constructor(client) {
        super(client, vscode_1.workspace.onDidSaveTextDocument, vscode_languageserver_protocol_1.DidSaveTextDocumentNotification.type, () => client.middleware.didSave, (textDocument) => client.code2ProtocolConverter.asSaveTextDocumentParams(textDocument, this._includeText), (data) => data, features_1.TextDocumentEventFeature.textDocumentFilter);
        this._includeText = false;
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidSaveTextDocumentNotification.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "synchronization").didSave = true;
      }
      initialize(capabilities, documentSelector) {
        const textDocumentSyncOptions = capabilities.resolvedTextDocumentSync;
        if (documentSelector && textDocumentSyncOptions && textDocumentSyncOptions.save) {
          const saveOptions = typeof textDocumentSyncOptions.save === "boolean" ? { includeText: false } : { includeText: !!textDocumentSyncOptions.save.includeText };
          this.register({
            id: UUID.generateUuid(),
            registerOptions: Object.assign({}, { documentSelector }, saveOptions)
          });
        }
      }
      register(data) {
        this._includeText = !!data.registerOptions.includeText;
        super.register(data);
      }
      getTextDocument(data) {
        return data;
      }
    };
    exports2.DidSaveTextDocumentFeature = DidSaveTextDocumentFeature;
    var USUAL_WORD_SEPARATORS = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";
    function createWordRegExp(allowInWords = "") {
      let source = "(-?\\d*\\.\\d\\w*)|([^";
      for (const sep of USUAL_WORD_SEPARATORS) {
        if (allowInWords.indexOf(sep) >= 0) {
          continue;
        }
        source += "\\" + sep;
      }
      source += "\\s]+)";
      return new RegExp(source, "g");
    }
    var DEFAULT_WORD_REGEXP = createWordRegExp();
    var TextDocumentSnapshot = class _TextDocumentSnapshot {
      _extTextDocument;
      _capturedTextDocument;
      _content;
      _uri;
      _fileName;
      _languageId;
      _version;
      _eol;
      _isUntitled;
      _encoding;
      _isDirty;
      _isClosed;
      constructor(textDocument) {
        this._extTextDocument = textDocument;
        this._content = textDocument.getText();
        this._uri = textDocument.uri;
        this._fileName = textDocument.fileName;
        this._languageId = textDocument.languageId;
        this._version = textDocument.version;
        this._eol = textDocument.eol;
        this._isUntitled = textDocument.isUntitled;
        this._encoding = textDocument.encoding;
        this._isDirty = textDocument.isDirty;
        this._isClosed = textDocument.isClosed;
        this._capturedTextDocument = vscode_languageserver_textdocument_1.TextDocument.create(this._uri.toString(), this._languageId, this._version, this._content);
      }
      get uri() {
        return this._uri;
      }
      get languageId() {
        return this._languageId;
      }
      get version() {
        return this._version;
      }
      get eol() {
        return this._eol;
      }
      get isUntitled() {
        return this._isUntitled;
      }
      get encoding() {
        return this._encoding;
      }
      get fileName() {
        return this._fileName;
      }
      get isDirty() {
        return this._isDirty;
      }
      get isClosed() {
        return this._isClosed;
      }
      save() {
        return this.version === this._extTextDocument.version ? this._extTextDocument.save() : Promise.resolve(false);
      }
      get lineCount() {
        return this._capturedTextDocument.lineCount;
      }
      offsetAt(position) {
        return this._capturedTextDocument.offsetAt(position);
      }
      positionAt(offset) {
        const position = this._capturedTextDocument.positionAt(offset);
        return new vscode_1.Position(position.line, position.character);
      }
      getText(range) {
        return this._capturedTextDocument.getText(range);
      }
      lineAt(lineOrPosition) {
        const line = typeof lineOrPosition === "number" ? lineOrPosition : this.validatePosition(lineOrPosition).line;
        if (line < 0 || line >= this.lineCount) {
          throw new RangeError(`Illegal value for line: ${line}`);
        }
        const lineRange = this._capturedTextDocument.getLineRange(line);
        const text = this._capturedTextDocument.getText(lineRange);
        const firstNonWhitespaceCharacterIndex = text.search(/\S/);
        const range = new vscode_1.Range(lineRange.start.line, lineRange.start.character, lineRange.end.line, lineRange.end.character);
        const rangeIncludingLineBreak = line + 1 < this.lineCount ? new vscode_1.Range(range.start.line, range.start.character, line + 1, 0) : range;
        return {
          lineNumber: line,
          text,
          range,
          rangeIncludingLineBreak,
          firstNonWhitespaceCharacterIndex: firstNonWhitespaceCharacterIndex === -1 ? text.length : firstNonWhitespaceCharacterIndex,
          isEmptyOrWhitespace: firstNonWhitespaceCharacterIndex === -1
        };
      }
      getWordRangeAtPosition(position, regex) {
        const lineNumber = this.validatePosition(position).line;
        const lineText = this.lineAt(lineNumber).text;
        const wordRegex = _TextDocumentSnapshot.getWordRegExp(regex);
        let match;
        wordRegex.lastIndex = 0;
        while ((match = wordRegex.exec(lineText)) !== null) {
          if (match.index <= position.character && wordRegex.lastIndex >= position.character) {
            return new vscode_1.Range(lineNumber, match.index, lineNumber, wordRegex.lastIndex);
          }
        }
        return void 0;
      }
      validateRange(range) {
        const start = this.validatePosition(range.start);
        const end = this.validatePosition(range.end);
        if (start === range.start && end === range.end) {
          return range;
        }
        return new vscode_1.Range(start.line, start.character, end.line, end.character);
      }
      validatePosition(position) {
        const line = Math.min(Math.max(position.line, 0), this.lineCount - 1);
        const lineRange = this._capturedTextDocument.getLineRange(line);
        const character = Math.min(Math.max(position.character, 0), lineRange.end.character);
        if (line === position.line && character === position.character) {
          return position;
        }
        return new vscode_1.Position(line, character);
      }
      static getWordRegExp(regex) {
        const result = regex ?? DEFAULT_WORD_REGEXP;
        if (result.flags.includes("g")) {
          return result;
        }
        const flags = `${result.flags}g`;
        return new RegExp(result.source, flags);
      }
    };
  }
});

// node_modules/vscode-languageclient/lib/common/completion.js
var require_completion = __commonJS({
  "node_modules/vscode-languageclient/lib/common/completion.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CompletionItemFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var SupportedCompletionItemKinds = [
      vscode_languageserver_protocol_1.CompletionItemKind.Text,
      vscode_languageserver_protocol_1.CompletionItemKind.Method,
      vscode_languageserver_protocol_1.CompletionItemKind.Function,
      vscode_languageserver_protocol_1.CompletionItemKind.Constructor,
      vscode_languageserver_protocol_1.CompletionItemKind.Field,
      vscode_languageserver_protocol_1.CompletionItemKind.Variable,
      vscode_languageserver_protocol_1.CompletionItemKind.Class,
      vscode_languageserver_protocol_1.CompletionItemKind.Interface,
      vscode_languageserver_protocol_1.CompletionItemKind.Module,
      vscode_languageserver_protocol_1.CompletionItemKind.Property,
      vscode_languageserver_protocol_1.CompletionItemKind.Unit,
      vscode_languageserver_protocol_1.CompletionItemKind.Value,
      vscode_languageserver_protocol_1.CompletionItemKind.Enum,
      vscode_languageserver_protocol_1.CompletionItemKind.Keyword,
      vscode_languageserver_protocol_1.CompletionItemKind.Snippet,
      vscode_languageserver_protocol_1.CompletionItemKind.Color,
      vscode_languageserver_protocol_1.CompletionItemKind.File,
      vscode_languageserver_protocol_1.CompletionItemKind.Reference,
      vscode_languageserver_protocol_1.CompletionItemKind.Folder,
      vscode_languageserver_protocol_1.CompletionItemKind.EnumMember,
      vscode_languageserver_protocol_1.CompletionItemKind.Constant,
      vscode_languageserver_protocol_1.CompletionItemKind.Struct,
      vscode_languageserver_protocol_1.CompletionItemKind.Event,
      vscode_languageserver_protocol_1.CompletionItemKind.Operator,
      vscode_languageserver_protocol_1.CompletionItemKind.TypeParameter
    ];
    var CompletionItemFeature = class extends features_1.TextDocumentLanguageFeature {
      labelDetailsSupport;
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.CompletionRequest.type);
        this.labelDetailsSupport = /* @__PURE__ */ new Map();
      }
      fillClientCapabilities(capabilities) {
        const completion = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "completion");
        completion.dynamicRegistration = true;
        completion.contextSupport = true;
        completion.completionItem = {
          snippetSupport: true,
          commitCharactersSupport: true,
          documentationFormat: [vscode_languageserver_protocol_1.MarkupKind.Markdown, vscode_languageserver_protocol_1.MarkupKind.PlainText],
          deprecatedSupport: true,
          preselectSupport: true,
          tagSupport: { valueSet: [vscode_languageserver_protocol_1.CompletionItemTag.Deprecated] },
          insertReplaceSupport: true,
          resolveSupport: {
            properties: ["documentation", "detail", "additionalTextEdits"]
          },
          insertTextModeSupport: { valueSet: [vscode_languageserver_protocol_1.InsertTextMode.asIs, vscode_languageserver_protocol_1.InsertTextMode.adjustIndentation] },
          labelDetailsSupport: true
        };
        completion.insertTextMode = vscode_languageserver_protocol_1.InsertTextMode.adjustIndentation;
        completion.completionItemKind = { valueSet: SupportedCompletionItemKinds };
        completion.completionList = {
          itemDefaults: [
            "commitCharacters",
            "editRange",
            "insertTextFormat",
            "insertTextMode",
            "data"
          ],
          applyKindSupport: true
        };
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.completionProvider);
        if (!options) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: options
        });
      }
      registerLanguageProvider(options, id) {
        this.labelDetailsSupport.set(id, !!options.completionItem?.labelDetailsSupport);
        const triggerCharacters = options.triggerCharacters ?? [];
        const defaultCommitCharacters = options.allCommitCharacters;
        const selector = options.documentSelector;
        const provider = {
          provideCompletionItems: (document, position, token, context) => {
            const client = this._client;
            const middleware = this._client.middleware;
            const provideCompletionItems = (document2, position2, context2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.CompletionRequest.type, client.code2ProtocolConverter.asCompletionParams(document2, position2, context2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asCompletionResult(result, defaultCommitCharacters, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CompletionRequest.type, token2, error, null);
              });
            };
            return middleware.provideCompletionItem ? middleware.provideCompletionItem(document, position, context, token, provideCompletionItems) : provideCompletionItems(document, position, context, token);
          },
          resolveCompletionItem: options.resolveProvider ? (item, token) => {
            const client = this._client;
            const middleware = this._client.middleware;
            const resolveCompletionItem = (item2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.CompletionResolveRequest.type, client.code2ProtocolConverter.asCompletionItem(item2, !!this.labelDetailsSupport.get(id)), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asCompletionItem(result);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CompletionResolveRequest.type, token2, error, item2);
              });
            };
            return middleware.resolveCompletionItem ? middleware.resolveCompletionItem(item, token, resolveCompletionItem) : resolveCompletionItem(item, token);
          } : void 0
        };
        return [vscode_1.languages.registerCompletionItemProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider, ...triggerCharacters), provider];
      }
    };
    exports2.CompletionItemFeature = CompletionItemFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/hover.js
var require_hover = __commonJS({
  "node_modules/vscode-languageclient/lib/common/hover.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.HoverFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var HoverFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.HoverRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const hoverCapability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "hover");
        hoverCapability.dynamicRegistration = true;
        hoverCapability.contentFormat = [vscode_languageserver_protocol_1.MarkupKind.Markdown, vscode_languageserver_protocol_1.MarkupKind.PlainText];
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.hoverProvider);
        if (!options) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: options
        });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideHover: (document, position, token) => {
            const client = this._client;
            const provideHover = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.HoverRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asHover(result);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.HoverRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideHover ? middleware.provideHover(document, position, token, provideHover) : provideHover(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerHoverProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.HoverFeature = HoverFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/definition.js
var require_definition = __commonJS({
  "node_modules/vscode-languageclient/lib/common/definition.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DefinitionFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var DefinitionFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DefinitionRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const definitionSupport = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "definition");
        definitionSupport.dynamicRegistration = true;
        definitionSupport.linkSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.definitionProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDefinition: (document, position, token) => {
            const client = this._client;
            const provideDefinition = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.DefinitionRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDefinitionResult(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DefinitionRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDefinition ? middleware.provideDefinition(document, position, token, provideDefinition) : provideDefinition(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerDefinitionProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.DefinitionFeature = DefinitionFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/signatureHelp.js
var require_signatureHelp = __commonJS({
  "node_modules/vscode-languageclient/lib/common/signatureHelp.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SignatureHelpFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var SignatureHelpFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.SignatureHelpRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const config = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "signatureHelp");
        config.dynamicRegistration = true;
        config.signatureInformation = { documentationFormat: [vscode_languageserver_protocol_1.MarkupKind.Markdown, vscode_languageserver_protocol_1.MarkupKind.PlainText] };
        config.signatureInformation.parameterInformation = { labelOffsetSupport: true };
        config.signatureInformation.activeParameterSupport = true;
        config.signatureInformation.noActiveParameterSupport = true;
        config.contextSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.signatureHelpProvider);
        if (!options) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: options
        });
      }
      registerLanguageProvider(options) {
        const provider = {
          provideSignatureHelp: (document, position, token, context) => {
            const client = this._client;
            const providerSignatureHelp = (document2, position2, context2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.SignatureHelpRequest.type, client.code2ProtocolConverter.asSignatureHelpParams(document2, position2, context2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asSignatureHelp(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.SignatureHelpRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideSignatureHelp ? middleware.provideSignatureHelp(document, position, context, token, providerSignatureHelp) : providerSignatureHelp(document, position, context, token);
          }
        };
        return [this.registerProvider(options, provider), provider];
      }
      registerProvider(options, provider) {
        const selector = this._client.protocol2CodeConverter.asDocumentSelector(options.documentSelector);
        if (options.retriggerCharacters === void 0) {
          const triggerCharacters = options.triggerCharacters || [];
          return vscode_1.languages.registerSignatureHelpProvider(selector, provider, ...triggerCharacters);
        } else {
          const metaData = {
            triggerCharacters: options.triggerCharacters || [],
            retriggerCharacters: options.retriggerCharacters || []
          };
          return vscode_1.languages.registerSignatureHelpProvider(selector, provider, metaData);
        }
      }
    };
    exports2.SignatureHelpFeature = SignatureHelpFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/documentHighlight.js
var require_documentHighlight = __commonJS({
  "node_modules/vscode-languageclient/lib/common/documentHighlight.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DocumentHighlightFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var DocumentHighlightFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentHighlightRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "documentHighlight").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentHighlightProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDocumentHighlights: (document, position, token) => {
            const client = this._client;
            const _provideDocumentHighlights = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentHighlightRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDocumentHighlights(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentHighlightRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentHighlights ? middleware.provideDocumentHighlights(document, position, token, _provideDocumentHighlights) : _provideDocumentHighlights(document, position, token);
          }
        };
        return [vscode_1.languages.registerDocumentHighlightProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.DocumentHighlightFeature = DocumentHighlightFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/documentSymbol.js
var require_documentSymbol = __commonJS({
  "node_modules/vscode-languageclient/lib/common/documentSymbol.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DocumentSymbolFeature = exports2.SupportedSymbolTags = exports2.SupportedSymbolKinds = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    exports2.SupportedSymbolKinds = [
      vscode_languageserver_protocol_1.SymbolKind.File,
      vscode_languageserver_protocol_1.SymbolKind.Module,
      vscode_languageserver_protocol_1.SymbolKind.Namespace,
      vscode_languageserver_protocol_1.SymbolKind.Package,
      vscode_languageserver_protocol_1.SymbolKind.Class,
      vscode_languageserver_protocol_1.SymbolKind.Method,
      vscode_languageserver_protocol_1.SymbolKind.Property,
      vscode_languageserver_protocol_1.SymbolKind.Field,
      vscode_languageserver_protocol_1.SymbolKind.Constructor,
      vscode_languageserver_protocol_1.SymbolKind.Enum,
      vscode_languageserver_protocol_1.SymbolKind.Interface,
      vscode_languageserver_protocol_1.SymbolKind.Function,
      vscode_languageserver_protocol_1.SymbolKind.Variable,
      vscode_languageserver_protocol_1.SymbolKind.Constant,
      vscode_languageserver_protocol_1.SymbolKind.String,
      vscode_languageserver_protocol_1.SymbolKind.Number,
      vscode_languageserver_protocol_1.SymbolKind.Boolean,
      vscode_languageserver_protocol_1.SymbolKind.Array,
      vscode_languageserver_protocol_1.SymbolKind.Object,
      vscode_languageserver_protocol_1.SymbolKind.Key,
      vscode_languageserver_protocol_1.SymbolKind.Null,
      vscode_languageserver_protocol_1.SymbolKind.EnumMember,
      vscode_languageserver_protocol_1.SymbolKind.Struct,
      vscode_languageserver_protocol_1.SymbolKind.Event,
      vscode_languageserver_protocol_1.SymbolKind.Operator,
      vscode_languageserver_protocol_1.SymbolKind.TypeParameter
    ];
    exports2.SupportedSymbolTags = [
      vscode_languageserver_protocol_1.SymbolTag.Deprecated
    ];
    var DocumentSymbolFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentSymbolRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const symbolCapabilities = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "documentSymbol");
        symbolCapabilities.dynamicRegistration = true;
        symbolCapabilities.symbolKind = {
          valueSet: exports2.SupportedSymbolKinds
        };
        symbolCapabilities.hierarchicalDocumentSymbolSupport = true;
        symbolCapabilities.tagSupport = {
          valueSet: exports2.SupportedSymbolTags
        };
        symbolCapabilities.labelSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentSymbolProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDocumentSymbols: (document, token) => {
            const client = this._client;
            const _provideDocumentSymbols = async (document2, token2) => {
              try {
                const data = await client.sendRequest(vscode_languageserver_protocol_1.DocumentSymbolRequest.type, client.code2ProtocolConverter.asDocumentSymbolParams(document2), token2);
                if (token2.isCancellationRequested || data === void 0 || data === null) {
                  return null;
                }
                if (data.length === 0) {
                  return [];
                } else {
                  const first = data[0];
                  if (vscode_languageserver_protocol_1.DocumentSymbol.is(first)) {
                    return await client.protocol2CodeConverter.asDocumentSymbols(data, token2);
                  } else {
                    return await client.protocol2CodeConverter.asSymbolInformations(data, token2);
                  }
                }
              } catch (error) {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentSymbolRequest.type, token2, error, null);
              }
            };
            const middleware = client.middleware;
            return middleware.provideDocumentSymbols ? middleware.provideDocumentSymbols(document, token, _provideDocumentSymbols) : _provideDocumentSymbols(document, token);
          }
        };
        const metaData = options.label !== void 0 ? { label: options.label } : void 0;
        return [vscode_1.languages.registerDocumentSymbolProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider, metaData), provider];
      }
    };
    exports2.DocumentSymbolFeature = DocumentSymbolFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/workspaceSymbol.js
var require_workspaceSymbol = __commonJS({
  "node_modules/vscode-languageclient/lib/common/workspaceSymbol.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WorkspaceSymbolFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var documentSymbol_1 = require_documentSymbol();
    var UUID = __importStar(require_uuid());
    var WorkspaceSymbolFeature = class extends features_1.WorkspaceFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.WorkspaceSymbolRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const symbolCapabilities = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "symbol");
        symbolCapabilities.dynamicRegistration = true;
        symbolCapabilities.symbolKind = {
          valueSet: documentSymbol_1.SupportedSymbolKinds
        };
        symbolCapabilities.tagSupport = {
          valueSet: documentSymbol_1.SupportedSymbolTags
        };
        symbolCapabilities.resolveSupport = { properties: ["location.range"] };
      }
      initialize(capabilities) {
        if (!capabilities.workspaceSymbolProvider) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: capabilities.workspaceSymbolProvider === true ? { workDoneProgress: false } : capabilities.workspaceSymbolProvider
        });
      }
      registerLanguageProvider(options) {
        const provider = {
          provideWorkspaceSymbols: (query, token) => {
            const client = this._client;
            const provideWorkspaceSymbols = (query2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.WorkspaceSymbolRequest.type, { query: query2 }, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asSymbolInformations(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.WorkspaceSymbolRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideWorkspaceSymbols ? middleware.provideWorkspaceSymbols(query, token, provideWorkspaceSymbols) : provideWorkspaceSymbols(query, token);
          },
          resolveWorkspaceSymbol: options.resolveProvider === true ? (item, token) => {
            const client = this._client;
            const resolveWorkspaceSymbol = (item2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.WorkspaceSymbolResolveRequest.type, client.code2ProtocolConverter.asWorkspaceSymbol(item2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asSymbolInformation(result);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.WorkspaceSymbolResolveRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.resolveWorkspaceSymbol ? middleware.resolveWorkspaceSymbol(item, token, resolveWorkspaceSymbol) : resolveWorkspaceSymbol(item, token);
          } : void 0
        };
        return [vscode_1.languages.registerWorkspaceSymbolProvider(provider), provider];
      }
    };
    exports2.WorkspaceSymbolFeature = WorkspaceSymbolFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/reference.js
var require_reference = __commonJS({
  "node_modules/vscode-languageclient/lib/common/reference.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ReferencesFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var ReferencesFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.ReferencesRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "references").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.referencesProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideReferences: (document, position, options2, token) => {
            const client = this._client;
            const _providerReferences = (document2, position2, options3, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.ReferencesRequest.type, client.code2ProtocolConverter.asReferenceParams(document2, position2, options3), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asReferences(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.ReferencesRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideReferences ? middleware.provideReferences(document, position, options2, token, _providerReferences) : _providerReferences(document, position, options2, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerReferenceProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.ReferencesFeature = ReferencesFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/typeDefinition.js
var require_typeDefinition = __commonJS({
  "node_modules/vscode-languageclient/lib/common/typeDefinition.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TypeDefinitionFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var TypeDefinitionFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.TypeDefinitionRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "typeDefinition").dynamicRegistration = true;
        const typeDefinitionSupport = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "typeDefinition");
        typeDefinitionSupport.dynamicRegistration = true;
        typeDefinitionSupport.linkSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.typeDefinitionProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideTypeDefinition: (document, position, token) => {
            const client = this._client;
            const provideTypeDefinition = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.TypeDefinitionRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDefinitionResult(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.TypeDefinitionRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideTypeDefinition ? middleware.provideTypeDefinition(document, position, token, provideTypeDefinition) : provideTypeDefinition(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerTypeDefinitionProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.TypeDefinitionFeature = TypeDefinitionFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/implementation.js
var require_implementation = __commonJS({
  "node_modules/vscode-languageclient/lib/common/implementation.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ImplementationFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var ImplementationFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.ImplementationRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const implementationSupport = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "implementation");
        implementationSupport.dynamicRegistration = true;
        implementationSupport.linkSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.implementationProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideImplementation: (document, position, token) => {
            const client = this._client;
            const provideImplementation = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.ImplementationRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDefinitionResult(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.ImplementationRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideImplementation ? middleware.provideImplementation(document, position, token, provideImplementation) : provideImplementation(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerImplementationProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.ImplementationFeature = ImplementationFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/colorProvider.js
var require_colorProvider = __commonJS({
  "node_modules/vscode-languageclient/lib/common/colorProvider.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ColorProviderFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var ColorProviderFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentColorRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "colorProvider").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.colorProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideColorPresentations: (color, context, token) => {
            const client = this._client;
            const provideColorPresentations = (color2, context2, token2) => {
              const requestParams = {
                color: color2,
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(context2.document),
                range: client.code2ProtocolConverter.asRange(context2.range)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.ColorPresentationRequest.type, requestParams, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return this._client.protocol2CodeConverter.asColorPresentations(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.ColorPresentationRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideColorPresentations ? middleware.provideColorPresentations(color, context, token, provideColorPresentations) : provideColorPresentations(color, context, token);
          },
          provideDocumentColors: (document, token) => {
            const client = this._client;
            const provideDocumentColors = (document2, token2) => {
              const requestParams = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentColorRequest.type, requestParams, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return this._client.protocol2CodeConverter.asColorInformations(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentColorRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentColors ? middleware.provideDocumentColors(document, token, provideDocumentColors) : provideDocumentColors(document, token);
          }
        };
        return [vscode_1.languages.registerColorProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.ColorProviderFeature = ColorProviderFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/codeAction.js
var require_codeAction = __commonJS({
  "node_modules/vscode-languageclient/lib/common/codeAction.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CodeActionFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var UUID = __importStar(require_uuid());
    var features_1 = require_features();
    var CodeActionFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.CodeActionRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const cap = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "codeAction");
        cap.dynamicRegistration = true;
        cap.isPreferredSupport = true;
        cap.disabledSupport = true;
        cap.dataSupport = true;
        cap.resolveSupport = {
          properties: ["edit", "command"]
        };
        cap.codeActionLiteralSupport = {
          codeActionKind: {
            valueSet: [
              vscode_languageserver_protocol_1.CodeActionKind.Empty,
              vscode_languageserver_protocol_1.CodeActionKind.QuickFix,
              vscode_languageserver_protocol_1.CodeActionKind.Refactor,
              vscode_languageserver_protocol_1.CodeActionKind.RefactorExtract,
              vscode_languageserver_protocol_1.CodeActionKind.RefactorInline,
              vscode_languageserver_protocol_1.CodeActionKind.RefactorMove,
              vscode_languageserver_protocol_1.CodeActionKind.RefactorRewrite,
              vscode_languageserver_protocol_1.CodeActionKind.Source,
              vscode_languageserver_protocol_1.CodeActionKind.SourceOrganizeImports,
              vscode_languageserver_protocol_1.CodeActionKind.Notebook
            ]
          }
        };
        cap.honorsChangeAnnotations = true;
        cap.documentationSupport = true;
        cap.tagSupport = {
          valueSet: [vscode_languageserver_protocol_1.CodeActionTag.LLMGenerated]
        };
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.codeActionProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideCodeActions: (document, range, context, token) => {
            const client = this._client;
            const _provideCodeActions = async (document2, range2, context2, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                range: client.code2ProtocolConverter.asRange(range2),
                context: client.code2ProtocolConverter.asCodeActionContextSync(context2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.CodeActionRequest.type, params, token2).then((values) => {
                if (token2.isCancellationRequested || values === null || values === void 0) {
                  return null;
                }
                return client.protocol2CodeConverter.asCodeActionResult(values, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CodeActionRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideCodeActions ? middleware.provideCodeActions(document, range, context, token, _provideCodeActions) : _provideCodeActions(document, range, context, token);
          },
          resolveCodeAction: options.resolveProvider ? (item, token) => {
            const client = this._client;
            const middleware = this._client.middleware;
            const resolveCodeAction = async (item2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.CodeActionResolveRequest.type, client.code2ProtocolConverter.asCodeActionSync(item2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return item2;
                }
                return client.protocol2CodeConverter.asCodeAction(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CodeActionResolveRequest.type, token2, error, item2);
              });
            };
            return middleware.resolveCodeAction ? middleware.resolveCodeAction(item, token, resolveCodeAction) : resolveCodeAction(item, token);
          } : void 0
        };
        return [vscode_1.languages.registerCodeActionsProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider, this.getMetadata(options)), provider];
      }
      getMetadata(options) {
        if (options.codeActionKinds === void 0 && options.documentation === void 0) {
          return void 0;
        }
        return {
          providedCodeActionKinds: this._client.protocol2CodeConverter.asCodeActionKinds(options.codeActionKinds),
          documentation: this._client.protocol2CodeConverter.asCodeActionDocumentations(options.documentation)
        };
      }
    };
    exports2.CodeActionFeature = CodeActionFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/codeLens.js
var require_codeLens = __commonJS({
  "node_modules/vscode-languageclient/lib/common/codeLens.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CodeLensFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var UUID = __importStar(require_uuid());
    var features_1 = require_features();
    var CodeLensFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.CodeLensRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const clc = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "codeLens");
        clc.dynamicRegistration = true;
        clc.resolveSupport = { properties: ["command"] };
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "codeLens").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.CodeLensRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeCodeLensEmitter.fire();
          }
        });
        const options = this.getRegistrationOptions(documentSelector, capabilities.codeLensProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const eventEmitter = new vscode_1.EventEmitter();
        const provider = {
          onDidChangeCodeLenses: eventEmitter.event,
          provideCodeLenses: (document, token) => {
            const client = this._client;
            const provideCodeLenses = (document2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.CodeLensRequest.type, client.code2ProtocolConverter.asCodeLensParams(document2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asCodeLenses(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CodeLensRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideCodeLenses ? middleware.provideCodeLenses(document, token, provideCodeLenses) : provideCodeLenses(document, token);
          },
          resolveCodeLens: options.resolveProvider ? (codeLens, token) => {
            const client = this._client;
            const resolveCodeLens = (codeLens2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.CodeLensResolveRequest.type, client.code2ProtocolConverter.asCodeLens(codeLens2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return codeLens2;
                }
                return client.protocol2CodeConverter.asCodeLens(result);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.CodeLensResolveRequest.type, token2, error, codeLens2);
              });
            };
            const middleware = client.middleware;
            return middleware.resolveCodeLens ? middleware.resolveCodeLens(codeLens, token, resolveCodeLens) : resolveCodeLens(codeLens, token);
          } : void 0
        };
        return [vscode_1.languages.registerCodeLensProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), { provider, onDidChangeCodeLensEmitter: eventEmitter }];
      }
    };
    exports2.CodeLensFeature = CodeLensFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/formatting.js
var require_formatting = __commonJS({
  "node_modules/vscode-languageclient/lib/common/formatting.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DocumentOnTypeFormattingFeature = exports2.DocumentRangeFormattingFeature = exports2.DocumentFormattingFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var UUID = __importStar(require_uuid());
    var features_1 = require_features();
    var FileFormattingOptions;
    (function(FileFormattingOptions2) {
      function fromConfiguration(document) {
        const filesConfig = vscode_1.workspace.getConfiguration("files", document);
        return {
          trimTrailingWhitespace: filesConfig.get("trimTrailingWhitespace"),
          trimFinalNewlines: filesConfig.get("trimFinalNewlines"),
          insertFinalNewline: filesConfig.get("insertFinalNewline")
        };
      }
      FileFormattingOptions2.fromConfiguration = fromConfiguration;
    })(FileFormattingOptions || (FileFormattingOptions = {}));
    var DocumentFormattingFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentFormattingRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "formatting").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentFormattingProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDocumentFormattingEdits: (document, options2, token) => {
            const client = this._client;
            const provideDocumentFormattingEdits = (document2, options3, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                options: client.code2ProtocolConverter.asFormattingOptions(options3, FileFormattingOptions.fromConfiguration(document2))
              };
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentFormattingRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asTextEdits(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentFormattingRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentFormattingEdits ? middleware.provideDocumentFormattingEdits(document, options2, token, provideDocumentFormattingEdits) : provideDocumentFormattingEdits(document, options2, token);
          }
        };
        return [vscode_1.languages.registerDocumentFormattingEditProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.DocumentFormattingFeature = DocumentFormattingFeature;
    var DocumentRangeFormattingFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentRangeFormattingRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "rangeFormatting");
        capability.dynamicRegistration = true;
        capability.rangesSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentRangeFormattingProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDocumentRangeFormattingEdits: (document, range, options2, token) => {
            const client = this._client;
            const provideDocumentRangeFormattingEdits = (document2, range2, options3, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                range: client.code2ProtocolConverter.asRange(range2),
                options: client.code2ProtocolConverter.asFormattingOptions(options3, FileFormattingOptions.fromConfiguration(document2))
              };
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentRangeFormattingRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asTextEdits(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentRangeFormattingRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentRangeFormattingEdits ? middleware.provideDocumentRangeFormattingEdits(document, range, options2, token, provideDocumentRangeFormattingEdits) : provideDocumentRangeFormattingEdits(document, range, options2, token);
          }
        };
        if (options.rangesSupport) {
          provider.provideDocumentRangesFormattingEdits = (document, ranges, options2, token) => {
            const client = this._client;
            const provideDocumentRangesFormattingEdits = (document2, ranges2, options3, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                ranges: client.code2ProtocolConverter.asRanges(ranges2),
                options: client.code2ProtocolConverter.asFormattingOptions(options3, FileFormattingOptions.fromConfiguration(document2))
              };
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentRangesFormattingRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asTextEdits(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentRangesFormattingRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentRangesFormattingEdits ? middleware.provideDocumentRangesFormattingEdits(document, ranges, options2, token, provideDocumentRangesFormattingEdits) : provideDocumentRangesFormattingEdits(document, ranges, options2, token);
          };
        }
        return [vscode_1.languages.registerDocumentRangeFormattingEditProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.DocumentRangeFormattingFeature = DocumentRangeFormattingFeature;
    var DocumentOnTypeFormattingFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentOnTypeFormattingRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "onTypeFormatting").dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentOnTypeFormattingProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideOnTypeFormattingEdits: (document, position, ch, options2, token) => {
            const client = this._client;
            const provideOnTypeFormattingEdits = (document2, position2, ch2, options3, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                position: client.code2ProtocolConverter.asPosition(position2),
                ch: ch2,
                options: client.code2ProtocolConverter.asFormattingOptions(options3, FileFormattingOptions.fromConfiguration(document2))
              };
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentOnTypeFormattingRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asTextEdits(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentOnTypeFormattingRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideOnTypeFormattingEdits ? middleware.provideOnTypeFormattingEdits(document, position, ch, options2, token, provideOnTypeFormattingEdits) : provideOnTypeFormattingEdits(document, position, ch, options2, token);
          }
        };
        const moreTriggerCharacter = options.moreTriggerCharacter || [];
        return [vscode_1.languages.registerOnTypeFormattingEditProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider, options.firstTriggerCharacter, ...moreTriggerCharacter), provider];
      }
    };
    exports2.DocumentOnTypeFormattingFeature = DocumentOnTypeFormattingFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/rename.js
var require_rename = __commonJS({
  "node_modules/vscode-languageclient/lib/common/rename.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.RenameFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var UUID = __importStar(require_uuid());
    var Is2 = __importStar(require_is());
    var features_1 = require_features();
    var RenameFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.RenameRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const rename = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "rename");
        rename.dynamicRegistration = true;
        rename.prepareSupport = true;
        rename.prepareSupportDefaultBehavior = vscode_languageserver_protocol_1.PrepareSupportDefaultBehavior.Identifier;
        rename.honorsChangeAnnotations = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.renameProvider);
        if (!options) {
          return;
        }
        if (Is2.boolean(capabilities.renameProvider)) {
          options.prepareProvider = false;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideRenameEdits: (document, position, newName, token) => {
            const client = this._client;
            const provideRenameEdits = async (document2, position2, newName2, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                position: client.code2ProtocolConverter.asPosition(position2),
                newName: newName2
              };
              let result = null;
              try {
                result = await client.sendRequest(vscode_languageserver_protocol_1.RenameRequest.type, params, token2);
              } catch (error) {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.RenameRequest.type, token2, error, null, false);
              }
              if (token2.isCancellationRequested || result === null) {
                return null;
              }
              const converted = await client.protocol2CodeConverter.asWorkspaceEdit(result, token2);
              if (token2.isCancellationRequested) {
                return null;
              }
              if (!client.validateWorkspaceEdit(result)) {
                throw new Error(`The rename edit returned from the server is not valid anymore and cannot be applied.`);
              }
              return converted;
            };
            const middleware = client.middleware;
            return middleware.provideRenameEdits ? middleware.provideRenameEdits(document, position, newName, token, provideRenameEdits) : provideRenameEdits(document, position, newName, token);
          },
          prepareRename: options.prepareProvider ? (document, position, token) => {
            const client = this._client;
            const prepareRename = (document2, position2, token2) => {
              const params = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                position: client.code2ProtocolConverter.asPosition(position2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.PrepareRenameRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                if (vscode_languageserver_protocol_1.Range.is(result)) {
                  return client.protocol2CodeConverter.asRange(result);
                } else if (this.isDefaultBehavior(result)) {
                  return result.defaultBehavior === true ? null : Promise.reject(new Error(`The element can't be renamed.`));
                } else if (result && vscode_languageserver_protocol_1.Range.is(result.range)) {
                  return {
                    range: client.protocol2CodeConverter.asRange(result.range),
                    placeholder: result.placeholder
                  };
                }
                return Promise.reject(new Error(`The element can't be renamed.`));
              }, (error) => {
                if (typeof error.message === "string") {
                  throw new Error(error.message);
                } else {
                  throw new Error(`The element can't be renamed.`);
                }
              });
            };
            const middleware = client.middleware;
            return middleware.prepareRename ? middleware.prepareRename(document, position, token, prepareRename) : prepareRename(document, position, token);
          } : void 0
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerRenameProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
      isDefaultBehavior(value) {
        const candidate = value;
        return candidate && Is2.boolean(candidate.defaultBehavior);
      }
    };
    exports2.RenameFeature = RenameFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/documentLink.js
var require_documentLink = __commonJS({
  "node_modules/vscode-languageclient/lib/common/documentLink.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DocumentLinkFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var DocumentLinkFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DocumentLinkRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const documentLinkCapabilities = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "documentLink");
        documentLinkCapabilities.dynamicRegistration = true;
        documentLinkCapabilities.tooltipSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.documentLinkProvider);
        if (!options) {
          return;
        }
        this.register({ id: UUID.generateUuid(), registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDocumentLinks: (document, token) => {
            const client = this._client;
            const provideDocumentLinks = (document2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentLinkRequest.type, client.code2ProtocolConverter.asDocumentLinkParams(document2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDocumentLinks(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentLinkRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDocumentLinks ? middleware.provideDocumentLinks(document, token, provideDocumentLinks) : provideDocumentLinks(document, token);
          },
          resolveDocumentLink: options.resolveProvider ? (link, token) => {
            const client = this._client;
            const resolveDocumentLink = (link2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.DocumentLinkResolveRequest.type, client.code2ProtocolConverter.asDocumentLink(link2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return link2;
                }
                return client.protocol2CodeConverter.asDocumentLink(result);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DocumentLinkResolveRequest.type, token2, error, link2);
              });
            };
            const middleware = client.middleware;
            return middleware.resolveDocumentLink ? middleware.resolveDocumentLink(link, token, resolveDocumentLink) : resolveDocumentLink(link, token);
          } : void 0
        };
        return [vscode_1.languages.registerDocumentLinkProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.DocumentLinkFeature = DocumentLinkFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/executeCommand.js
var require_executeCommand = __commonJS({
  "node_modules/vscode-languageclient/lib/common/executeCommand.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ExecuteCommandFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var UUID = __importStar(require_uuid());
    var features_1 = require_features();
    var ExecuteCommandFeature = class {
      _client;
      _commands;
      constructor(client) {
        this._client = client;
        this._commands = /* @__PURE__ */ new Map();
      }
      getState() {
        return { kind: "workspace", id: this.registrationType.method, registrations: this._commands.size > 0 };
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.ExecuteCommandRequest.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "executeCommand").dynamicRegistration = true;
      }
      initialize(capabilities) {
        if (!capabilities.executeCommandProvider) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: Object.assign({}, capabilities.executeCommandProvider)
        });
      }
      register(data) {
        const client = this._client;
        const middleware = client.middleware;
        const executeCommand = (command, args) => {
          const params = {
            command,
            arguments: args
          };
          return client.sendRequest(vscode_languageserver_protocol_1.ExecuteCommandRequest.type, params).then(void 0, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.ExecuteCommandRequest.type, void 0, error, void 0);
          });
        };
        if (data.registerOptions.commands) {
          const disposables = [];
          for (const command of data.registerOptions.commands) {
            disposables.push(vscode_1.commands.registerCommand(command, (...args) => {
              return middleware.executeCommand ? middleware.executeCommand(command, args, executeCommand) : executeCommand(command, args);
            }));
          }
          this._commands.set(data.id, disposables);
        }
      }
      unregister(id) {
        const disposables = this._commands.get(id);
        if (disposables) {
          this._commands.delete(id);
          disposables.forEach((disposable) => disposable.dispose());
        }
      }
      clear() {
        this._commands.forEach((value) => {
          value.forEach((disposable) => disposable.dispose());
        });
        this._commands.clear();
      }
    };
    exports2.ExecuteCommandFeature = ExecuteCommandFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/foldingRange.js
var require_foldingRange = __commonJS({
  "node_modules/vscode-languageclient/lib/common/foldingRange.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.FoldingRangeFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var FoldingRangeFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.FoldingRangeRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "foldingRange");
        capability.dynamicRegistration = true;
        capability.rangeLimit = 5e3;
        capability.lineFoldingOnly = true;
        capability.foldingRangeKind = { valueSet: [vscode_languageserver_protocol_1.FoldingRangeKind.Comment, vscode_languageserver_protocol_1.FoldingRangeKind.Imports, vscode_languageserver_protocol_1.FoldingRangeKind.Region] };
        capability.foldingRange = { collapsedText: false };
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "foldingRange").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        this._client.onRequest(vscode_languageserver_protocol_1.FoldingRangeRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeFoldingRange.fire();
          }
        });
        const [id, options] = this.getRegistration(documentSelector, capabilities.foldingRangeProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const eventEmitter = new vscode_1.EventEmitter();
        const provider = {
          onDidChangeFoldingRanges: eventEmitter.event,
          provideFoldingRanges: (document, context, token) => {
            const client = this._client;
            const provideFoldingRanges = (document2, _, token2) => {
              const requestParams = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.FoldingRangeRequest.type, requestParams, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asFoldingRanges(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.FoldingRangeRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideFoldingRanges ? middleware.provideFoldingRanges(document, context, token, provideFoldingRanges) : provideFoldingRanges(document, context, token);
          }
        };
        return [vscode_1.languages.registerFoldingRangeProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), { provider, onDidChangeFoldingRange: eventEmitter }];
      }
    };
    exports2.FoldingRangeFeature = FoldingRangeFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/declaration.js
var require_declaration = __commonJS({
  "node_modules/vscode-languageclient/lib/common/declaration.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DeclarationFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var DeclarationFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.DeclarationRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const declarationSupport = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "declaration");
        declarationSupport.dynamicRegistration = true;
        declarationSupport.linkSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.declarationProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideDeclaration: (document, position, token) => {
            const client = this._client;
            const provideDeclaration = (document2, position2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.DeclarationRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asDeclarationResult(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.DeclarationRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideDeclaration ? middleware.provideDeclaration(document, position, token, provideDeclaration) : provideDeclaration(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerDeclarationProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.DeclarationFeature = DeclarationFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/selectionRange.js
var require_selectionRange = __commonJS({
  "node_modules/vscode-languageclient/lib/common/selectionRange.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SelectionRangeFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var SelectionRangeFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.SelectionRangeRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "selectionRange");
        capability.dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.selectionRangeProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideSelectionRanges: (document, positions, token) => {
            const client = this._client;
            const provideSelectionRanges = async (document2, positions2, token2) => {
              const requestParams = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                positions: client.code2ProtocolConverter.asPositionsSync(positions2, token2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.SelectionRangeRequest.type, requestParams, token2).then((ranges) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asSelectionRanges(ranges, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.SelectionRangeRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideSelectionRanges ? middleware.provideSelectionRanges(document, positions, token, provideSelectionRanges) : provideSelectionRanges(document, positions, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerSelectionRangeProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.SelectionRangeFeature = SelectionRangeFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/callHierarchy.js
var require_callHierarchy = __commonJS({
  "node_modules/vscode-languageclient/lib/common/callHierarchy.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.CallHierarchyFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var CallHierarchyProvider = class {
      client;
      middleware;
      constructor(client) {
        this.client = client;
        this.middleware = client.middleware;
      }
      prepareCallHierarchy(document, position, token) {
        const client = this.client;
        const middleware = this.middleware;
        const prepareCallHierarchy = (document2, position2, token2) => {
          const params = client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2);
          return client.sendRequest(vscode_languageserver_protocol_1.CallHierarchyPrepareRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asCallHierarchyItems(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.CallHierarchyPrepareRequest.type, token2, error, null);
          });
        };
        return middleware.prepareCallHierarchy ? middleware.prepareCallHierarchy(document, position, token, prepareCallHierarchy) : prepareCallHierarchy(document, position, token);
      }
      provideCallHierarchyIncomingCalls(item, token) {
        const client = this.client;
        const middleware = this.middleware;
        const provideCallHierarchyIncomingCalls = (item2, token2) => {
          const params = {
            item: client.code2ProtocolConverter.asCallHierarchyItem(item2)
          };
          return client.sendRequest(vscode_languageserver_protocol_1.CallHierarchyIncomingCallsRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asCallHierarchyIncomingCalls(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.CallHierarchyIncomingCallsRequest.type, token2, error, null);
          });
        };
        return middleware.provideCallHierarchyIncomingCalls ? middleware.provideCallHierarchyIncomingCalls(item, token, provideCallHierarchyIncomingCalls) : provideCallHierarchyIncomingCalls(item, token);
      }
      provideCallHierarchyOutgoingCalls(item, token) {
        const client = this.client;
        const middleware = this.middleware;
        const provideCallHierarchyOutgoingCalls = (item2, token2) => {
          const params = {
            item: client.code2ProtocolConverter.asCallHierarchyItem(item2)
          };
          return client.sendRequest(vscode_languageserver_protocol_1.CallHierarchyOutgoingCallsRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asCallHierarchyOutgoingCalls(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.CallHierarchyOutgoingCallsRequest.type, token2, error, null);
          });
        };
        return middleware.provideCallHierarchyOutgoingCalls ? middleware.provideCallHierarchyOutgoingCalls(item, token, provideCallHierarchyOutgoingCalls) : provideCallHierarchyOutgoingCalls(item, token);
      }
    };
    var CallHierarchyFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.CallHierarchyPrepareRequest.type);
      }
      fillClientCapabilities(cap) {
        const capabilities = cap;
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "callHierarchy");
        capability.dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.callHierarchyProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const client = this._client;
        const provider = new CallHierarchyProvider(client);
        return [vscode_1.languages.registerCallHierarchyProvider(this._client.protocol2CodeConverter.asDocumentSelector(options.documentSelector), provider), provider];
      }
    };
    exports2.CallHierarchyFeature = CallHierarchyFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/semanticTokens.js
var require_semanticTokens = __commonJS({
  "node_modules/vscode-languageclient/lib/common/semanticTokens.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SemanticTokensFeature = void 0;
    var vscode2 = __importStar(require("vscode"));
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var Is2 = __importStar(require_is());
    var SemanticTokensFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.SemanticTokensRegistrationType.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "semanticTokens");
        capability.dynamicRegistration = true;
        capability.tokenTypes = [
          vscode_languageserver_protocol_1.SemanticTokenTypes.namespace,
          vscode_languageserver_protocol_1.SemanticTokenTypes.type,
          vscode_languageserver_protocol_1.SemanticTokenTypes.class,
          vscode_languageserver_protocol_1.SemanticTokenTypes.enum,
          vscode_languageserver_protocol_1.SemanticTokenTypes.interface,
          vscode_languageserver_protocol_1.SemanticTokenTypes.struct,
          vscode_languageserver_protocol_1.SemanticTokenTypes.typeParameter,
          vscode_languageserver_protocol_1.SemanticTokenTypes.parameter,
          vscode_languageserver_protocol_1.SemanticTokenTypes.variable,
          vscode_languageserver_protocol_1.SemanticTokenTypes.property,
          vscode_languageserver_protocol_1.SemanticTokenTypes.enumMember,
          vscode_languageserver_protocol_1.SemanticTokenTypes.event,
          vscode_languageserver_protocol_1.SemanticTokenTypes.function,
          vscode_languageserver_protocol_1.SemanticTokenTypes.method,
          vscode_languageserver_protocol_1.SemanticTokenTypes.macro,
          vscode_languageserver_protocol_1.SemanticTokenTypes.keyword,
          vscode_languageserver_protocol_1.SemanticTokenTypes.comment,
          vscode_languageserver_protocol_1.SemanticTokenTypes.string,
          vscode_languageserver_protocol_1.SemanticTokenTypes.number,
          vscode_languageserver_protocol_1.SemanticTokenTypes.regexp,
          vscode_languageserver_protocol_1.SemanticTokenTypes.operator,
          vscode_languageserver_protocol_1.SemanticTokenTypes.decorator,
          vscode_languageserver_protocol_1.SemanticTokenTypes.label
        ];
        capability.tokenModifiers = [
          vscode_languageserver_protocol_1.SemanticTokenModifiers.declaration,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.definition,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.readonly,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.static,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.deprecated,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.abstract,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.async,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.modification,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.documentation,
          vscode_languageserver_protocol_1.SemanticTokenModifiers.defaultLibrary
        ];
        capability.formats = [vscode_languageserver_protocol_1.TokenFormat.Relative];
        capability.requests = {
          range: true,
          full: {
            delta: true
          }
        };
        capability.multilineTokenSupport = false;
        capability.overlappingTokenSupport = false;
        capability.serverCancelSupport = true;
        capability.augmentsSyntaxTokens = true;
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "semanticTokens").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.SemanticTokensRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeSemanticTokensEmitter.fire();
          }
        });
        const [id, options] = this.getRegistration(documentSelector, capabilities.semanticTokensProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const fullProvider = Is2.boolean(options.full) ? options.full : options.full !== void 0;
        const hasEditProvider = options.full !== void 0 && typeof options.full !== "boolean" && options.full.delta === true;
        const eventEmitter = new vscode2.EventEmitter();
        const documentProvider = fullProvider ? {
          onDidChangeSemanticTokens: eventEmitter.event,
          provideDocumentSemanticTokens: (document, token) => {
            const client2 = this._client;
            const middleware = client2.middleware;
            const provideDocumentSemanticTokens = (document2, token2) => {
              const params = {
                textDocument: client2.code2ProtocolConverter.asTextDocumentIdentifier(document2)
              };
              return client2.sendRequest(vscode_languageserver_protocol_1.SemanticTokensRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client2.protocol2CodeConverter.asSemanticTokens(result, token2);
              }, (error) => {
                return client2.handleFailedRequest(vscode_languageserver_protocol_1.SemanticTokensRequest.type, token2, error, null);
              });
            };
            return middleware.provideDocumentSemanticTokens ? middleware.provideDocumentSemanticTokens(document, token, provideDocumentSemanticTokens) : provideDocumentSemanticTokens(document, token);
          },
          provideDocumentSemanticTokensEdits: hasEditProvider ? (document, previousResultId, token) => {
            const client2 = this._client;
            const middleware = client2.middleware;
            const provideDocumentSemanticTokensEdits = (document2, previousResultId2, token2) => {
              const params = {
                textDocument: client2.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                previousResultId: previousResultId2
              };
              return client2.sendRequest(vscode_languageserver_protocol_1.SemanticTokensDeltaRequest.type, params, token2).then(async (result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                if (vscode_languageserver_protocol_1.SemanticTokens.is(result)) {
                  return await client2.protocol2CodeConverter.asSemanticTokens(result, token2);
                } else {
                  return await client2.protocol2CodeConverter.asSemanticTokensEdits(result, token2);
                }
              }, (error) => {
                return client2.handleFailedRequest(vscode_languageserver_protocol_1.SemanticTokensDeltaRequest.type, token2, error, null);
              });
            };
            return middleware.provideDocumentSemanticTokensEdits ? middleware.provideDocumentSemanticTokensEdits(document, previousResultId, token, provideDocumentSemanticTokensEdits) : provideDocumentSemanticTokensEdits(document, previousResultId, token);
          } : void 0
        } : void 0;
        const hasRangeProvider = options.range === true;
        const rangeProvider = hasRangeProvider ? {
          onDidChangeSemanticTokens: eventEmitter.event,
          provideDocumentRangeSemanticTokens: (document, range, token) => {
            const client2 = this._client;
            const middleware = client2.middleware;
            const provideDocumentRangeSemanticTokens = (document2, range2, token2) => {
              const params = {
                textDocument: client2.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                range: client2.code2ProtocolConverter.asRange(range2)
              };
              return client2.sendRequest(vscode_languageserver_protocol_1.SemanticTokensRangeRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client2.protocol2CodeConverter.asSemanticTokens(result, token2);
              }, (error) => {
                return client2.handleFailedRequest(vscode_languageserver_protocol_1.SemanticTokensRangeRequest.type, token2, error, null);
              });
            };
            return middleware.provideDocumentRangeSemanticTokens ? middleware.provideDocumentRangeSemanticTokens(document, range, token, provideDocumentRangeSemanticTokens) : provideDocumentRangeSemanticTokens(document, range, token);
          }
        } : void 0;
        const disposables = [];
        const client = this._client;
        const legend = client.protocol2CodeConverter.asSemanticTokensLegend(options.legend);
        const documentSelector = client.protocol2CodeConverter.asDocumentSelector(selector);
        if (documentProvider !== void 0) {
          disposables.push(vscode2.languages.registerDocumentSemanticTokensProvider(documentSelector, documentProvider, legend));
        }
        if (rangeProvider !== void 0) {
          disposables.push(vscode2.languages.registerDocumentRangeSemanticTokensProvider(documentSelector, rangeProvider, legend));
        }
        return [new vscode2.Disposable(() => disposables.forEach((item) => item.dispose())), { range: rangeProvider, full: documentProvider, onDidChangeSemanticTokensEmitter: eventEmitter }];
      }
    };
    exports2.SemanticTokensFeature = SemanticTokensFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/linkedEditingRange.js
var require_linkedEditingRange = __commonJS({
  "node_modules/vscode-languageclient/lib/common/linkedEditingRange.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.LinkedEditingFeature = void 0;
    var code = __importStar(require("vscode"));
    var proto = __importStar(require_api2());
    var features_1 = require_features();
    var LinkedEditingFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, proto.LinkedEditingRangeRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const linkedEditingSupport = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "linkedEditingRange");
        linkedEditingSupport.dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.linkedEditingRangeProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideLinkedEditingRanges: (document, position, token) => {
            const client = this._client;
            const provideLinkedEditing = (document2, position2, token2) => {
              return client.sendRequest(proto.LinkedEditingRangeRequest.type, client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asLinkedEditingRanges(result, token2);
              }, (error) => {
                return client.handleFailedRequest(proto.LinkedEditingRangeRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideLinkedEditingRange ? middleware.provideLinkedEditingRange(document, position, token, provideLinkedEditing) : provideLinkedEditing(document, position, token);
          }
        };
        return [this.registerProvider(selector, provider), provider];
      }
      registerProvider(selector, provider) {
        return code.languages.registerLinkedEditingRangeProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.LinkedEditingFeature = LinkedEditingFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/typeHierarchy.js
var require_typeHierarchy = __commonJS({
  "node_modules/vscode-languageclient/lib/common/typeHierarchy.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TypeHierarchyFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var TypeHierarchyProvider = class {
      client;
      middleware;
      constructor(client) {
        this.client = client;
        this.middleware = client.middleware;
      }
      prepareTypeHierarchy(document, position, token) {
        const client = this.client;
        const middleware = this.middleware;
        const prepareTypeHierarchy = (document2, position2, token2) => {
          const params = client.code2ProtocolConverter.asTextDocumentPositionParams(document2, position2);
          return client.sendRequest(vscode_languageserver_protocol_1.TypeHierarchyPrepareRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asTypeHierarchyItems(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.TypeHierarchyPrepareRequest.type, token2, error, null);
          });
        };
        return middleware.prepareTypeHierarchy ? middleware.prepareTypeHierarchy(document, position, token, prepareTypeHierarchy) : prepareTypeHierarchy(document, position, token);
      }
      provideTypeHierarchySupertypes(item, token) {
        const client = this.client;
        const middleware = this.middleware;
        const provideTypeHierarchySupertypes = (item2, token2) => {
          const params = {
            item: client.code2ProtocolConverter.asTypeHierarchyItem(item2)
          };
          return client.sendRequest(vscode_languageserver_protocol_1.TypeHierarchySupertypesRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asTypeHierarchyItems(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.TypeHierarchySupertypesRequest.type, token2, error, null);
          });
        };
        return middleware.provideTypeHierarchySupertypes ? middleware.provideTypeHierarchySupertypes(item, token, provideTypeHierarchySupertypes) : provideTypeHierarchySupertypes(item, token);
      }
      provideTypeHierarchySubtypes(item, token) {
        const client = this.client;
        const middleware = this.middleware;
        const provideTypeHierarchySubtypes = (item2, token2) => {
          const params = {
            item: client.code2ProtocolConverter.asTypeHierarchyItem(item2)
          };
          return client.sendRequest(vscode_languageserver_protocol_1.TypeHierarchySubtypesRequest.type, params, token2).then((result) => {
            if (token2.isCancellationRequested) {
              return null;
            }
            return client.protocol2CodeConverter.asTypeHierarchyItems(result, token2);
          }, (error) => {
            return client.handleFailedRequest(vscode_languageserver_protocol_1.TypeHierarchySubtypesRequest.type, token2, error, null);
          });
        };
        return middleware.provideTypeHierarchySubtypes ? middleware.provideTypeHierarchySubtypes(item, token, provideTypeHierarchySubtypes) : provideTypeHierarchySubtypes(item, token);
      }
    };
    var TypeHierarchyFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.TypeHierarchyPrepareRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const capability = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "typeHierarchy");
        capability.dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const [id, options] = this.getRegistration(documentSelector, capabilities.typeHierarchyProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const client = this._client;
        const provider = new TypeHierarchyProvider(client);
        return [vscode_1.languages.registerTypeHierarchyProvider(client.protocol2CodeConverter.asDocumentSelector(options.documentSelector), provider), provider];
      }
    };
    exports2.TypeHierarchyFeature = TypeHierarchyFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/inlineValue.js
var require_inlineValue = __commonJS({
  "node_modules/vscode-languageclient/lib/common/inlineValue.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlineValueFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var InlineValueFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.InlineValueRequest.type);
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "inlineValue").dynamicRegistration = true;
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "inlineValue").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        this._client.onRequest(vscode_languageserver_protocol_1.InlineValueRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeInlineValues.fire();
          }
        });
        const [id, options] = this.getRegistration(documentSelector, capabilities.inlineValueProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const eventEmitter = new vscode_1.EventEmitter();
        const provider = {
          onDidChangeInlineValues: eventEmitter.event,
          provideInlineValues: (document, viewPort, context, token) => {
            const client = this._client;
            const provideInlineValues = (document2, viewPort2, context2, token2) => {
              const requestParams = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                range: client.code2ProtocolConverter.asRange(viewPort2),
                context: client.code2ProtocolConverter.asInlineValueContext(context2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.InlineValueRequest.type, requestParams, token2).then((values) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asInlineValues(values, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.InlineValueRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideInlineValues ? middleware.provideInlineValues(document, viewPort, context, token, provideInlineValues) : provideInlineValues(document, viewPort, context, token);
          }
        };
        return [this.registerProvider(selector, provider), { provider, onDidChangeInlineValues: eventEmitter }];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerInlineValuesProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.InlineValueFeature = InlineValueFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/inlayHint.js
var require_inlayHint = __commonJS({
  "node_modules/vscode-languageclient/lib/common/inlayHint.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlayHintsFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var InlayHintsFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.InlayHintRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const inlayHint = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "inlayHint");
        inlayHint.dynamicRegistration = true;
        inlayHint.resolveSupport = {
          properties: ["tooltip", "textEdits", "label.tooltip", "label.location", "label.command"]
        };
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "inlayHint").refreshSupport = true;
      }
      initialize(capabilities, documentSelector) {
        this._client.onRequest(vscode_languageserver_protocol_1.InlayHintRefreshRequest.type, async () => {
          for (const provider of this.getAllProviders()) {
            provider.onDidChangeInlayHints.fire();
          }
        });
        const [id, options] = this.getRegistration(documentSelector, capabilities.inlayHintProvider);
        if (!id || !options) {
          return;
        }
        this.register({ id, registerOptions: options });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const eventEmitter = new vscode_1.EventEmitter();
        const provider = {
          onDidChangeInlayHints: eventEmitter.event,
          provideInlayHints: (document, viewPort, token) => {
            const client = this._client;
            const provideInlayHints = async (document2, viewPort2, token2) => {
              const requestParams = {
                textDocument: client.code2ProtocolConverter.asTextDocumentIdentifier(document2),
                range: client.code2ProtocolConverter.asRange(viewPort2)
              };
              try {
                const values = await client.sendRequest(vscode_languageserver_protocol_1.InlayHintRequest.type, requestParams, token2);
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asInlayHints(values, token2);
              } catch (error) {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.InlayHintRequest.type, token2, error, null);
              }
            };
            const middleware = client.middleware;
            return middleware.provideInlayHints ? middleware.provideInlayHints(document, viewPort, token, provideInlayHints) : provideInlayHints(document, viewPort, token);
          }
        };
        provider.resolveInlayHint = options.resolveProvider === true ? (hint, token) => {
          const client = this._client;
          const resolveInlayHint = async (item, token2) => {
            try {
              const value = await client.sendRequest(vscode_languageserver_protocol_1.InlayHintResolveRequest.type, client.code2ProtocolConverter.asInlayHint(item), token2);
              if (token2.isCancellationRequested) {
                return null;
              }
              const result = client.protocol2CodeConverter.asInlayHint(value, token2);
              return token2.isCancellationRequested ? null : result;
            } catch (error) {
              return client.handleFailedRequest(vscode_languageserver_protocol_1.InlayHintResolveRequest.type, token2, error, null);
            }
          };
          const middleware = client.middleware;
          return middleware.resolveInlayHint ? middleware.resolveInlayHint(hint, token, resolveInlayHint) : resolveInlayHint(hint, token);
        } : void 0;
        return [this.registerProvider(selector, provider), { provider, onDidChangeInlayHints: eventEmitter }];
      }
      registerProvider(selector, provider) {
        return vscode_1.languages.registerInlayHintsProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider);
      }
    };
    exports2.InlayHintsFeature = InlayHintsFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/workspaceFolder.js
var require_workspaceFolder = __commonJS({
  "node_modules/vscode-languageclient/lib/common/workspaceFolder.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WorkspaceFoldersFeature = void 0;
    exports2.arrayDiff = arrayDiff;
    var UUID = __importStar(require_uuid());
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    function access(target, key) {
      if (target === void 0 || target === null) {
        return void 0;
      }
      return target[key];
    }
    function arrayDiff(left, right) {
      return left.filter((element) => right.indexOf(element) < 0);
    }
    var WorkspaceFoldersFeature = class {
      _client;
      _listeners;
      _initialFolders;
      constructor(client) {
        this._client = client;
        this._listeners = /* @__PURE__ */ new Map();
      }
      getState() {
        return { kind: "workspace", id: this.registrationType.method, registrations: this._listeners.size > 0 };
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidChangeWorkspaceFoldersNotification.type;
      }
      fillInitializeParams(params) {
        const folders = vscode_1.workspace.workspaceFolders;
        this.initializeWithFolders(folders);
        if (folders === void 0) {
          params.workspaceFolders = null;
        } else {
          params.workspaceFolders = folders.map((folder) => this.asProtocol(folder));
        }
      }
      initializeWithFolders(currentWorkspaceFolders) {
        this._initialFolders = currentWorkspaceFolders;
      }
      fillClientCapabilities(capabilities) {
        capabilities.workspace = capabilities.workspace || {};
        capabilities.workspace.workspaceFolders = true;
      }
      initialize(capabilities) {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.WorkspaceFoldersRequest.type, (token) => {
          const workspaceFolders = () => {
            const folders = vscode_1.workspace.workspaceFolders;
            if (folders === void 0) {
              return null;
            }
            const result = folders.map((folder) => {
              return this.asProtocol(folder);
            });
            return result;
          };
          const middleware = client.middleware.workspace;
          return middleware && middleware.workspaceFolders ? middleware.workspaceFolders(token, workspaceFolders) : workspaceFolders(token);
        });
        const value = access(access(access(capabilities, "workspace"), "workspaceFolders"), "changeNotifications");
        let id;
        if (typeof value === "string") {
          id = value;
        } else if (value === true) {
          id = UUID.generateUuid();
        }
        if (id) {
          this.register({ id, registerOptions: void 0 });
        }
      }
      sendInitialEvent(currentWorkspaceFolders) {
        let promise;
        if (this._initialFolders && currentWorkspaceFolders) {
          const removed = arrayDiff(this._initialFolders, currentWorkspaceFolders);
          const added = arrayDiff(currentWorkspaceFolders, this._initialFolders);
          if (added.length > 0 || removed.length > 0) {
            promise = this.doSendEvent(added, removed);
          }
        } else if (this._initialFolders) {
          promise = this.doSendEvent([], this._initialFolders);
        } else if (currentWorkspaceFolders) {
          promise = this.doSendEvent(currentWorkspaceFolders, []);
        }
        if (promise !== void 0) {
          promise.catch((error) => {
            this._client.error(`Sending notification ${vscode_languageserver_protocol_1.DidChangeWorkspaceFoldersNotification.type.method} failed`, error);
          });
        }
      }
      doSendEvent(addedFolders, removedFolders) {
        const params = {
          event: {
            added: addedFolders.map((folder) => this.asProtocol(folder)),
            removed: removedFolders.map((folder) => this.asProtocol(folder))
          }
        };
        return this._client.sendNotification(vscode_languageserver_protocol_1.DidChangeWorkspaceFoldersNotification.type, params);
      }
      register(data) {
        const id = data.id;
        const client = this._client;
        const disposable = vscode_1.workspace.onDidChangeWorkspaceFolders((event) => {
          const didChangeWorkspaceFolders = (event2) => {
            return this.doSendEvent(event2.added, event2.removed);
          };
          const middleware = client.middleware.workspace;
          const promise = middleware && middleware.didChangeWorkspaceFolders ? middleware.didChangeWorkspaceFolders(event, didChangeWorkspaceFolders) : didChangeWorkspaceFolders(event);
          promise.catch((error) => {
            this._client.error(`Sending notification ${vscode_languageserver_protocol_1.DidChangeWorkspaceFoldersNotification.type.method} failed`, error);
          });
        });
        this._listeners.set(id, disposable);
        this.sendInitialEvent(vscode_1.workspace.workspaceFolders);
      }
      unregister(id) {
        const disposable = this._listeners.get(id);
        if (disposable === void 0) {
          return;
        }
        this._listeners.delete(id);
        disposable.dispose();
      }
      clear() {
        for (const disposable of this._listeners.values()) {
          disposable.dispose();
        }
        this._listeners.clear();
      }
      asProtocol(workspaceFolder) {
        if (workspaceFolder === void 0) {
          return null;
        }
        return { uri: this._client.code2ProtocolConverter.asUri(workspaceFolder.uri), name: workspaceFolder.name };
      }
    };
    exports2.WorkspaceFoldersFeature = WorkspaceFoldersFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/fileOperations.js
var require_fileOperations = __commonJS({
  "node_modules/vscode-languageclient/lib/common/fileOperations.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.WillDeleteFilesFeature = exports2.WillRenameFilesFeature = exports2.WillCreateFilesFeature = exports2.DidDeleteFilesFeature = exports2.DidRenameFilesFeature = exports2.DidCreateFilesFeature = void 0;
    var code = __importStar(require("vscode"));
    var minimatch = __importStar(require_commonjs3());
    var proto = __importStar(require_api2());
    var UUID = __importStar(require_uuid());
    function ensure(target, key) {
      if (target[key] === void 0) {
        target[key] = {};
      }
      return target[key];
    }
    function access(target, key) {
      return target[key];
    }
    function assign(target, key, value) {
      target[key] = value;
    }
    var FileOperationFeature = class _FileOperationFeature {
      _client;
      _event;
      _registrationType;
      _clientCapability;
      _serverCapability;
      _listener;
      // This property must stay private. Otherwise the type `minimatch.IMinimatch` becomes public and as a consequence we would need to
      // ship the d.ts files for minimatch to make the compiler happy when compiling against the vscode-languageclient library
      _filters;
      constructor(client, event, registrationType, clientCapability, serverCapability) {
        this._client = client;
        this._event = event;
        this._registrationType = registrationType;
        this._clientCapability = clientCapability;
        this._serverCapability = serverCapability;
        this._filters = /* @__PURE__ */ new Map();
      }
      getState() {
        return { kind: "workspace", id: this._registrationType.method, registrations: this._filters.size > 0 };
      }
      filterSize() {
        return this._filters.size;
      }
      get registrationType() {
        return this._registrationType;
      }
      fillClientCapabilities(capabilities) {
        const value = ensure(ensure(capabilities, "workspace"), "fileOperations");
        assign(value, "dynamicRegistration", true);
        assign(value, this._clientCapability, true);
      }
      initialize(capabilities) {
        const options = capabilities.workspace?.fileOperations;
        const capability = options !== void 0 ? access(options, this._serverCapability) : void 0;
        if (capability?.filters !== void 0) {
          try {
            this.register({
              id: UUID.generateUuid(),
              registerOptions: { filters: capability.filters }
            });
          } catch (e) {
            this._client.warn(`Ignoring invalid glob pattern for ${this._serverCapability} registration: ${e}`);
          }
        }
      }
      register(data) {
        if (!this._listener) {
          this._listener = this._event(this.send, this);
        }
        const minimatchFilter = data.registerOptions.filters.map((filter) => {
          const matcher = new minimatch.Minimatch(filter.pattern.glob, _FileOperationFeature.asMinimatchOptions(filter.pattern.options));
          if (!matcher.makeRe()) {
            throw new Error(`Invalid pattern ${filter.pattern.glob}!`);
          }
          return { scheme: filter.scheme, matcher, kind: filter.pattern.matches };
        });
        this._filters.set(data.id, minimatchFilter);
      }
      unregister(id) {
        this._filters.delete(id);
        if (this._filters.size === 0 && this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      clear() {
        this._filters.clear();
        if (this._listener) {
          this._listener.dispose();
          this._listener = void 0;
        }
      }
      getFileType(uri) {
        return _FileOperationFeature.getFileType(uri);
      }
      async filter(event, prop) {
        const fileMatches = await Promise.all(event.files.map(async (item) => {
          const uri = prop(item);
          const path = uri.fsPath.replace(/\\/g, "/");
          for (const filters of this._filters.values()) {
            for (const filter of filters) {
              if (filter.scheme !== void 0 && filter.scheme !== uri.scheme) {
                continue;
              }
              if (filter.matcher.match(path)) {
                if (filter.kind === void 0) {
                  return true;
                }
                const fileType = await this.getFileType(uri);
                if (fileType === void 0) {
                  this._client.info(`Unable to determine file type for ${uri.toString()}. Treating as a match.`);
                  return true;
                }
                if (fileType === code.FileType.File && filter.kind === proto.FileOperationPatternKind.file || fileType === code.FileType.Directory && filter.kind === proto.FileOperationPatternKind.folder) {
                  return true;
                }
              } else if (filter.kind === proto.FileOperationPatternKind.folder) {
                const fileType = await _FileOperationFeature.getFileType(uri);
                if (fileType === code.FileType.Directory && filter.matcher.match(`${path}/`)) {
                  return true;
                }
              }
            }
          }
          return false;
        }));
        const files = event.files.filter((_, index) => fileMatches[index]);
        return { ...event, files };
      }
      static async getFileType(uri) {
        try {
          return (await code.workspace.fs.stat(uri)).type;
        } catch (e) {
          return void 0;
        }
      }
      static asMinimatchOptions(options) {
        const result = { dot: true };
        if (options?.ignoreCase === true) {
          result.nocase = true;
        }
        return result;
      }
    };
    var NotificationFileOperationFeature = class extends FileOperationFeature {
      _notificationType;
      _accessUri;
      _createParams;
      constructor(client, event, notificationType, clientCapability, serverCapability, accessUri, createParams) {
        super(client, event, notificationType, clientCapability, serverCapability);
        this._notificationType = notificationType;
        this._accessUri = accessUri;
        this._createParams = createParams;
      }
      async send(originalEvent) {
        const filteredEvent = await this.filter(originalEvent, this._accessUri);
        if (filteredEvent.files.length) {
          const next = async (event) => {
            return this._client.sendNotification(this._notificationType, this._createParams(event));
          };
          return this.doSend(filteredEvent, next);
        }
      }
    };
    var CachingNotificationFileOperationFeature = class extends NotificationFileOperationFeature {
      _willListener;
      _fsPathFileTypes = /* @__PURE__ */ new Map();
      async getFileType(uri) {
        const fsPath = uri.fsPath;
        if (this._fsPathFileTypes.has(fsPath)) {
          return this._fsPathFileTypes.get(fsPath);
        }
        const type = await FileOperationFeature.getFileType(uri);
        if (type) {
          this._fsPathFileTypes.set(fsPath, type);
        }
        return type;
      }
      async cacheFileTypes(event, prop) {
        await this.filter(event, prop);
      }
      clearFileTypeCache() {
        this._fsPathFileTypes.clear();
      }
      unregister(id) {
        super.unregister(id);
        if (this.filterSize() === 0 && this._willListener) {
          this._willListener.dispose();
          this._willListener = void 0;
        }
      }
      clear() {
        super.clear();
        if (this._willListener) {
          this._willListener.dispose();
          this._willListener = void 0;
        }
      }
    };
    var DidCreateFilesFeature = class extends NotificationFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onDidCreateFiles, proto.DidCreateFilesNotification.type, "didCreate", "didCreate", (i) => i, client.code2ProtocolConverter.asDidCreateFilesParams);
      }
      doSend(event, next) {
        const middleware = this._client.middleware.workspace;
        return middleware?.didCreateFiles ? middleware.didCreateFiles(event, next) : next(event);
      }
    };
    exports2.DidCreateFilesFeature = DidCreateFilesFeature;
    var DidRenameFilesFeature = class extends CachingNotificationFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onDidRenameFiles, proto.DidRenameFilesNotification.type, "didRename", "didRename", (i) => i.oldUri, client.code2ProtocolConverter.asDidRenameFilesParams);
      }
      register(data) {
        if (!this._willListener) {
          this._willListener = code.workspace.onWillRenameFiles(this.willRename, this);
        }
        super.register(data);
      }
      willRename(e) {
        e.waitUntil(this.cacheFileTypes(e, (i) => i.oldUri));
      }
      doSend(event, next) {
        this.clearFileTypeCache();
        const middleware = this._client.middleware.workspace;
        return middleware?.didRenameFiles ? middleware.didRenameFiles(event, next) : next(event);
      }
    };
    exports2.DidRenameFilesFeature = DidRenameFilesFeature;
    var DidDeleteFilesFeature = class extends CachingNotificationFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onDidDeleteFiles, proto.DidDeleteFilesNotification.type, "didDelete", "didDelete", (i) => i, client.code2ProtocolConverter.asDidDeleteFilesParams);
      }
      register(data) {
        if (!this._willListener) {
          this._willListener = code.workspace.onWillDeleteFiles(this.willDelete, this);
        }
        super.register(data);
      }
      willDelete(e) {
        e.waitUntil(this.cacheFileTypes(e, (i) => i));
      }
      doSend(event, next) {
        this.clearFileTypeCache();
        const middleware = this._client.middleware.workspace;
        return middleware?.didDeleteFiles ? middleware.didDeleteFiles(event, next) : next(event);
      }
    };
    exports2.DidDeleteFilesFeature = DidDeleteFilesFeature;
    var RequestFileOperationFeature = class extends FileOperationFeature {
      _requestType;
      _accessUri;
      _createParams;
      constructor(client, event, requestType, clientCapability, serverCapability, accessUri, createParams) {
        super(client, event, requestType, clientCapability, serverCapability);
        this._requestType = requestType;
        this._accessUri = accessUri;
        this._createParams = createParams;
      }
      async send(originalEvent) {
        const waitUntil = this.waitUntil(originalEvent);
        originalEvent.waitUntil(waitUntil);
      }
      async waitUntil(originalEvent) {
        const filteredEvent = await this.filter(originalEvent, this._accessUri);
        if (filteredEvent.files.length) {
          const next = (event) => {
            return this._client.sendRequest(this._requestType, this._createParams(event), event.token).then(this._client.protocol2CodeConverter.asWorkspaceEdit);
          };
          return this.doSend(filteredEvent, next);
        } else {
          return void 0;
        }
      }
    };
    var WillCreateFilesFeature = class extends RequestFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onWillCreateFiles, proto.WillCreateFilesRequest.type, "willCreate", "willCreate", (i) => i, client.code2ProtocolConverter.asWillCreateFilesParams);
      }
      doSend(event, next) {
        const middleware = this._client.middleware.workspace;
        return middleware?.willCreateFiles ? middleware.willCreateFiles(event, next) : next(event);
      }
    };
    exports2.WillCreateFilesFeature = WillCreateFilesFeature;
    var WillRenameFilesFeature = class extends RequestFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onWillRenameFiles, proto.WillRenameFilesRequest.type, "willRename", "willRename", (i) => i.oldUri, client.code2ProtocolConverter.asWillRenameFilesParams);
      }
      doSend(event, next) {
        const middleware = this._client.middleware.workspace;
        return middleware?.willRenameFiles ? middleware.willRenameFiles(event, next) : next(event);
      }
    };
    exports2.WillRenameFilesFeature = WillRenameFilesFeature;
    var WillDeleteFilesFeature = class extends RequestFileOperationFeature {
      constructor(client) {
        super(client, code.workspace.onWillDeleteFiles, proto.WillDeleteFilesRequest.type, "willDelete", "willDelete", (i) => i, client.code2ProtocolConverter.asWillDeleteFilesParams);
      }
      doSend(event, next) {
        const middleware = this._client.middleware.workspace;
        return middleware?.willDeleteFiles ? middleware.willDeleteFiles(event, next) : next(event);
      }
    };
    exports2.WillDeleteFilesFeature = WillDeleteFilesFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/inlineCompletion.js
var require_inlineCompletion = __commonJS({
  "node_modules/vscode-languageclient/lib/common/inlineCompletion.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.InlineCompletionItemFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var InlineCompletionItemFeature = class extends features_1.TextDocumentLanguageFeature {
      constructor(client) {
        super(client, vscode_languageserver_protocol_1.InlineCompletionRequest.type);
      }
      fillClientCapabilities(capabilities) {
        const inlineCompletion = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "textDocument"), "inlineCompletion");
        inlineCompletion.dynamicRegistration = true;
      }
      initialize(capabilities, documentSelector) {
        const options = this.getRegistrationOptions(documentSelector, capabilities.inlineCompletionProvider);
        if (!options) {
          return;
        }
        this.register({
          id: UUID.generateUuid(),
          registerOptions: options
        });
      }
      registerLanguageProvider(options) {
        const selector = options.documentSelector;
        const provider = {
          provideInlineCompletionItems: (document, position, context, token) => {
            const client = this._client;
            const middleware = this._client.middleware;
            const provideInlineCompletionItems = (document2, position2, context2, token2) => {
              return client.sendRequest(vscode_languageserver_protocol_1.InlineCompletionRequest.type, client.code2ProtocolConverter.asInlineCompletionParams(document2, position2, context2), token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return client.protocol2CodeConverter.asInlineCompletionResult(result, token2);
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.InlineCompletionRequest.type, token2, error, null);
              });
            };
            return middleware.provideInlineCompletionItems ? middleware.provideInlineCompletionItems(document, position, context, token, provideInlineCompletionItems) : provideInlineCompletionItems(document, position, context, token);
          }
        };
        return [vscode_1.languages.registerInlineCompletionItemProvider(this._client.protocol2CodeConverter.asDocumentSelector(selector), provider), provider];
      }
    };
    exports2.InlineCompletionItemFeature = InlineCompletionItemFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/textDocumentContent.js
var require_textDocumentContent = __commonJS({
  "node_modules/vscode-languageclient/lib/common/textDocumentContent.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.TextDocumentContentFeature = void 0;
    var vscode2 = __importStar(require("vscode"));
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var UUID = __importStar(require_uuid());
    var TextDocumentContentFeature = class {
      _client;
      _registrations = /* @__PURE__ */ new Map();
      constructor(client) {
        this._client = client;
      }
      getState() {
        const registrations = this._registrations.size > 0;
        return { kind: "workspace", id: vscode_languageserver_protocol_1.TextDocumentContentRequest.method, registrations };
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.TextDocumentContentRequest.type;
      }
      getProviders() {
        const result = [];
        for (const registration of this._registrations.values()) {
          result.push(...registration.providers);
        }
        return result;
      }
      fillClientCapabilities(capabilities) {
        const textDocumentContent = (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "textDocumentContent");
        textDocumentContent.dynamicRegistration = true;
      }
      initialize(capabilities) {
        const client = this._client;
        client.onRequest(vscode_languageserver_protocol_1.TextDocumentContentRefreshRequest.type, async (params) => {
          const uri = client.protocol2CodeConverter.asUri(params.uri);
          for (const registrations of this._registrations.values()) {
            for (const provider of registrations.providers) {
              if (provider.scheme === uri.scheme) {
                provider.onDidChangeEmitter.fire(uri);
              }
            }
          }
        });
        if (!capabilities?.workspace?.textDocumentContent) {
          return;
        }
        const capability = capabilities.workspace.textDocumentContent;
        const id = vscode_languageserver_protocol_1.StaticRegistrationOptions.hasId(capability) ? capability.id : UUID.generateUuid();
        this.register({
          id,
          registerOptions: capability
        });
      }
      register(data) {
        const registrations = [];
        const disposables = [];
        for (const scheme of data.registerOptions.schemes) {
          const [disposable, registration] = this.registerTextDocumentContentProvider(scheme);
          registrations.push(registration);
          disposables.push(disposable);
        }
        this._registrations.set(data.id, { disposable: vscode2.Disposable.from(...disposables), providers: registrations });
      }
      registerTextDocumentContentProvider(scheme) {
        const eventEmitter = new vscode2.EventEmitter();
        const provider = {
          onDidChange: eventEmitter.event,
          provideTextDocumentContent: (uri, token) => {
            const client = this._client;
            const provideTextDocumentContent = (uri2, token2) => {
              const params = {
                uri: client.code2ProtocolConverter.asUri(uri2)
              };
              return client.sendRequest(vscode_languageserver_protocol_1.TextDocumentContentRequest.type, params, token2).then((result) => {
                if (token2.isCancellationRequested) {
                  return null;
                }
                return result.text;
              }, (error) => {
                return client.handleFailedRequest(vscode_languageserver_protocol_1.TextDocumentContentRequest.type, token2, error, null);
              });
            };
            const middleware = client.middleware;
            return middleware.provideTextDocumentContent ? middleware.provideTextDocumentContent(uri, token, provideTextDocumentContent) : provideTextDocumentContent(uri, token);
          }
        };
        return [vscode2.workspace.registerTextDocumentContentProvider(scheme, provider), { scheme, onDidChangeEmitter: eventEmitter, provider }];
      }
      unregister(id) {
        const registration = this._registrations.get(id);
        if (registration !== void 0) {
          this._registrations.delete(id);
          registration.disposable.dispose();
        }
      }
      clear() {
        this._registrations.forEach((registration) => {
          registration.disposable.dispose();
        });
        this._registrations.clear();
      }
    };
    exports2.TextDocumentContentFeature = TextDocumentContentFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/fileSystemWatcher.js
var require_fileSystemWatcher = __commonJS({
  "node_modules/vscode-languageclient/lib/common/fileSystemWatcher.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.FileSystemWatcherFeature = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var features_1 = require_features();
    var FileSystemWatcherFeature = class {
      _client;
      _notifyFileEvent;
      _watchers;
      constructor(client, notifyFileEvent) {
        this._client = client;
        this._notifyFileEvent = notifyFileEvent;
        this._watchers = /* @__PURE__ */ new Map();
      }
      getState() {
        return { kind: "workspace", id: this.registrationType.method, registrations: this._watchers.size > 0 };
      }
      get registrationType() {
        return vscode_languageserver_protocol_1.DidChangeWatchedFilesNotification.type;
      }
      fillClientCapabilities(capabilities) {
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "didChangeWatchedFiles").dynamicRegistration = true;
        (0, features_1.ensure)((0, features_1.ensure)(capabilities, "workspace"), "didChangeWatchedFiles").relativePatternSupport = true;
      }
      initialize(_capabilities, _documentSelector) {
      }
      register(data) {
        if (!Array.isArray(data.registerOptions.watchers)) {
          return;
        }
        const disposables = [];
        for (const watcher of data.registerOptions.watchers) {
          const globPattern = this._client.protocol2CodeConverter.asGlobPattern(watcher.globPattern);
          if (globPattern === void 0) {
            continue;
          }
          let watchCreate = true, watchChange = true, watchDelete = true;
          if (watcher.kind !== void 0 && watcher.kind !== null) {
            watchCreate = (watcher.kind & vscode_languageserver_protocol_1.WatchKind.Create) !== 0;
            watchChange = (watcher.kind & vscode_languageserver_protocol_1.WatchKind.Change) !== 0;
            watchDelete = (watcher.kind & vscode_languageserver_protocol_1.WatchKind.Delete) !== 0;
          }
          const fileSystemWatcher = vscode_1.workspace.createFileSystemWatcher(globPattern, !watchCreate, !watchChange, !watchDelete);
          this.hookListeners(fileSystemWatcher, watchCreate, watchChange, watchDelete, disposables);
          disposables.push(fileSystemWatcher);
        }
        this._watchers.set(data.id, disposables);
      }
      registerRaw(id, fileSystemWatchers) {
        const disposables = [];
        for (const fileSystemWatcher of fileSystemWatchers) {
          this.hookListeners(fileSystemWatcher, true, true, true, disposables);
        }
        this._watchers.set(id, disposables);
      }
      hookListeners(fileSystemWatcher, watchCreate, watchChange, watchDelete, listeners) {
        if (watchCreate) {
          fileSystemWatcher.onDidCreate((resource) => this._notifyFileEvent({
            uri: this._client.code2ProtocolConverter.asUri(resource),
            type: vscode_languageserver_protocol_1.FileChangeType.Created
          }), null, listeners);
        }
        if (watchChange) {
          fileSystemWatcher.onDidChange((resource) => this._notifyFileEvent({
            uri: this._client.code2ProtocolConverter.asUri(resource),
            type: vscode_languageserver_protocol_1.FileChangeType.Changed
          }), null, listeners);
        }
        if (watchDelete) {
          fileSystemWatcher.onDidDelete((resource) => this._notifyFileEvent({
            uri: this._client.code2ProtocolConverter.asUri(resource),
            type: vscode_languageserver_protocol_1.FileChangeType.Deleted
          }), null, listeners);
        }
      }
      unregister(id) {
        const disposables = this._watchers.get(id);
        if (disposables) {
          this._watchers.delete(id);
          for (const disposable of disposables) {
            disposable.dispose();
          }
        }
      }
      clear() {
        this._watchers.forEach((disposables) => {
          for (const disposable of disposables) {
            disposable.dispose();
          }
        });
        this._watchers.clear();
      }
    };
    exports2.FileSystemWatcherFeature = FileSystemWatcherFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/progress.js
var require_progress = __commonJS({
  "node_modules/vscode-languageclient/lib/common/progress.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ProgressFeature = void 0;
    var vscode_languageserver_protocol_1 = require_api2();
    var progressPart_1 = require_progressPart();
    function ensure(target, key) {
      if (target[key] === void 0) {
        target[key] = /* @__PURE__ */ Object.create(null);
      }
      return target[key];
    }
    var ProgressFeature = class {
      _client;
      activeParts;
      constructor(_client) {
        this._client = _client;
        this.activeParts = /* @__PURE__ */ new Set();
      }
      getState() {
        return { kind: "window", id: vscode_languageserver_protocol_1.WorkDoneProgressCreateRequest.method, registrations: this.activeParts.size > 0 };
      }
      fillClientCapabilities(capabilities) {
        ensure(capabilities, "window").workDoneProgress = true;
      }
      initialize() {
        const client = this._client;
        const deleteHandler = (part) => {
          this.activeParts.delete(part);
        };
        const createHandler = (params) => {
          this.activeParts.add(new progressPart_1.ProgressPart(this._client, params.token, deleteHandler));
        };
        client.onRequest(vscode_languageserver_protocol_1.WorkDoneProgressCreateRequest.type, createHandler);
      }
      clear() {
        for (const part of this.activeParts) {
          part.done();
        }
        this.activeParts.clear();
      }
    };
    exports2.ProgressFeature = ProgressFeature;
  }
});

// node_modules/vscode-languageclient/lib/common/client.js
var require_client = __commonJS({
  "node_modules/vscode-languageclient/lib/common/client.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.ProposedFeatures = exports2.LanguageClient = exports2.BaseLanguageClient = exports2.ShutdownMode = exports2.MessageTransports = exports2.SuspendMode = exports2.State = exports2.CloseAction = exports2.ErrorAction = exports2.RevealOutputChannelOn = void 0;
    var vscode_1 = require("vscode");
    var vscode_languageserver_protocol_1 = require_api2();
    var c2p = __importStar(require_codeConverter());
    var p2c = __importStar(require_protocolConverter());
    var Is2 = __importStar(require_is());
    var async_1 = require_async();
    var UUID = __importStar(require_uuid());
    var progressPart_1 = require_progressPart();
    var features_1 = require_features();
    var diagnostic_1 = require_diagnostic();
    var notebook_1 = require_notebook();
    var configuration_1 = require_configuration();
    var textSynchronization_1 = require_textSynchronization();
    var completion_1 = require_completion();
    var hover_1 = require_hover();
    var definition_1 = require_definition();
    var signatureHelp_1 = require_signatureHelp();
    var documentHighlight_1 = require_documentHighlight();
    var documentSymbol_1 = require_documentSymbol();
    var workspaceSymbol_1 = require_workspaceSymbol();
    var reference_1 = require_reference();
    var typeDefinition_1 = require_typeDefinition();
    var implementation_1 = require_implementation();
    var colorProvider_1 = require_colorProvider();
    var codeAction_1 = require_codeAction();
    var codeLens_1 = require_codeLens();
    var formatting_1 = require_formatting();
    var rename_1 = require_rename();
    var documentLink_1 = require_documentLink();
    var executeCommand_1 = require_executeCommand();
    var foldingRange_1 = require_foldingRange();
    var declaration_1 = require_declaration();
    var selectionRange_1 = require_selectionRange();
    var callHierarchy_1 = require_callHierarchy();
    var semanticTokens_1 = require_semanticTokens();
    var linkedEditingRange_1 = require_linkedEditingRange();
    var typeHierarchy_1 = require_typeHierarchy();
    var inlineValue_1 = require_inlineValue();
    var inlayHint_1 = require_inlayHint();
    var workspaceFolder_1 = require_workspaceFolder();
    var fileOperations_1 = require_fileOperations();
    var inlineCompletion_1 = require_inlineCompletion();
    var textDocumentContent_1 = require_textDocumentContent();
    var fileSystemWatcher_1 = require_fileSystemWatcher();
    var progress_1 = require_progress();
    var RevealOutputChannelOn;
    (function(RevealOutputChannelOn2) {
      RevealOutputChannelOn2[RevealOutputChannelOn2["Debug"] = 0] = "Debug";
      RevealOutputChannelOn2[RevealOutputChannelOn2["Info"] = 1] = "Info";
      RevealOutputChannelOn2[RevealOutputChannelOn2["Warn"] = 2] = "Warn";
      RevealOutputChannelOn2[RevealOutputChannelOn2["Error"] = 3] = "Error";
      RevealOutputChannelOn2[RevealOutputChannelOn2["Never"] = 4] = "Never";
    })(RevealOutputChannelOn || (exports2.RevealOutputChannelOn = RevealOutputChannelOn = {}));
    var ErrorAction;
    (function(ErrorAction2) {
      ErrorAction2[ErrorAction2["Continue"] = 1] = "Continue";
      ErrorAction2[ErrorAction2["Shutdown"] = 2] = "Shutdown";
    })(ErrorAction || (exports2.ErrorAction = ErrorAction = {}));
    var CloseAction;
    (function(CloseAction2) {
      CloseAction2[CloseAction2["DoNotRestart"] = 1] = "DoNotRestart";
      CloseAction2[CloseAction2["Restart"] = 2] = "Restart";
    })(CloseAction || (exports2.CloseAction = CloseAction = {}));
    var State2;
    (function(State3) {
      State3[State3["Stopped"] = 1] = "Stopped";
      State3[State3["Starting"] = 3] = "Starting";
      State3[State3["StartFailed"] = 4] = "StartFailed";
      State3[State3["Running"] = 2] = "Running";
    })(State2 || (exports2.State = State2 = {}));
    var SuspendMode;
    (function(SuspendMode2) {
      SuspendMode2["off"] = "off";
      SuspendMode2["on"] = "on";
    })(SuspendMode || (exports2.SuspendMode = SuspendMode = {}));
    var ResolvedClientOptions;
    (function(ResolvedClientOptions2) {
      function sanitizeIsTrusted(isTrusted) {
        if (isTrusted === void 0 || isTrusted === null) {
          return false;
        }
        if (typeof isTrusted === "boolean" || typeof isTrusted === "object" && isTrusted !== null && Is2.stringArray(isTrusted.enabledCommands)) {
          return isTrusted;
        }
        return false;
      }
      ResolvedClientOptions2.sanitizeIsTrusted = sanitizeIsTrusted;
    })(ResolvedClientOptions || (ResolvedClientOptions = {}));
    var DefaultErrorHandler = class {
      client;
      maxRestartCount;
      restarts;
      constructor(client, maxRestartCount) {
        this.client = client;
        this.maxRestartCount = maxRestartCount;
        this.restarts = [];
      }
      error(_error, _message, count) {
        if (count && count <= 3) {
          return { action: ErrorAction.Continue };
        }
        return { action: ErrorAction.Shutdown };
      }
      closed() {
        this.restarts.push(Date.now());
        if (this.restarts.length <= this.maxRestartCount) {
          return { action: CloseAction.Restart };
        } else {
          const diff = this.restarts[this.restarts.length - 1] - this.restarts[0];
          if (diff <= 3 * 60 * 1e3) {
            return { action: CloseAction.DoNotRestart, message: `The ${this.client.name} server crashed ${this.maxRestartCount + 1} times in the last 3 minutes. The server will not be restarted. See the output for more information.` };
          } else {
            this.restarts.shift();
            return { action: CloseAction.Restart };
          }
        }
      }
    };
    var ClientState;
    (function(ClientState2) {
      ClientState2["Initial"] = "initial";
      ClientState2["Starting"] = "starting";
      ClientState2["StartFailed"] = "startFailed";
      ClientState2["Running"] = "running";
      ClientState2["Stopping"] = "stopping";
      ClientState2["Stopped"] = "stopped";
    })(ClientState || (ClientState = {}));
    var MessageTransports;
    (function(MessageTransports2) {
      function is(value) {
        const candidate = value;
        return candidate && vscode_languageserver_protocol_1.MessageReader.is(value.reader) && vscode_languageserver_protocol_1.MessageWriter.is(value.writer);
      }
      MessageTransports2.is = is;
    })(MessageTransports || (exports2.MessageTransports = MessageTransports = {}));
    var ShutdownMode;
    (function(ShutdownMode2) {
      ShutdownMode2["Restart"] = "restart";
      ShutdownMode2["Stop"] = "stop";
    })(ShutdownMode || (exports2.ShutdownMode = ShutdownMode = {}));
    var VisibleDocumentsImpl = class _VisibleDocumentsImpl {
      open;
      _onOpen;
      _onClose;
      disposables;
      constructor() {
        this.disposables = [];
        this.open = /* @__PURE__ */ new Set();
        this._onOpen = new vscode_1.EventEmitter();
        this._onClose = new vscode_1.EventEmitter();
        _VisibleDocumentsImpl.fillVisibleResources(this.open);
        const updateVisibleDocuments = () => {
          const oldTabs = this.open;
          const currentTabs = /* @__PURE__ */ new Set();
          _VisibleDocumentsImpl.fillVisibleResources(currentTabs);
          const closed = /* @__PURE__ */ new Set();
          const opened = new Set(currentTabs);
          for (const tab of oldTabs.values()) {
            if (currentTabs.has(tab)) {
              opened.delete(tab);
            } else {
              closed.add(tab);
            }
          }
          this.open = currentTabs;
          if (closed.size > 0) {
            const toFire = /* @__PURE__ */ new Set();
            for (const item of closed) {
              toFire.add(vscode_1.Uri.parse(item));
            }
            this._onClose.fire(toFire);
          }
          if (opened.size > 0) {
            const toFire = /* @__PURE__ */ new Set();
            for (const item of opened) {
              toFire.add(vscode_1.Uri.parse(item));
            }
            this._onOpen.fire(toFire);
          }
        };
        this.disposables.push(vscode_1.window.tabGroups.onDidChangeTabs((event) => {
          if (event.closed.length === 0 && event.opened.length === 0) {
            return;
          }
          updateVisibleDocuments();
        }));
        this.disposables.push(vscode_1.window.onDidChangeVisibleTextEditors((_editors) => {
          updateVisibleDocuments();
        }));
      }
      get onClose() {
        return this._onClose.event;
      }
      get onOpen() {
        return this._onOpen.event;
      }
      dispose() {
        this.disposables.forEach((disposable) => disposable.dispose());
      }
      isActive(document) {
        return document instanceof vscode_1.Uri ? vscode_1.window.activeTextEditor?.document.uri === document : vscode_1.window.activeTextEditor?.document === document;
      }
      isVisible(document) {
        const uri = document instanceof vscode_1.Uri ? document : document.uri;
        if (uri.scheme === notebook_1.NotebookDocumentSyncFeature.CellScheme) {
          return vscode_1.workspace.notebookDocuments.some((notebook) => {
            if (this.open.has(notebook.uri.toString())) {
              const cell = notebook.getCells().find((cell2) => cell2.document.uri.toString() === uri.toString());
              return cell !== void 0;
            }
            return false;
          });
        }
        return this.open.has(uri.toString());
      }
      getResources() {
        const result = /* @__PURE__ */ new Set();
        _VisibleDocumentsImpl.fillVisibleResources(/* @__PURE__ */ new Set(), result);
        return result;
      }
      static fillVisibleResources(strings, uris) {
        const seen = strings ?? /* @__PURE__ */ new Set();
        for (const group of vscode_1.window.tabGroups.all) {
          for (const tab of group.tabs) {
            const input = tab.input;
            let uri;
            if (input instanceof vscode_1.TabInputText) {
              uri = input.uri;
            } else if (input instanceof vscode_1.TabInputTextDiff) {
              uri = input.modified;
            } else if (input instanceof vscode_1.TabInputCustom) {
              uri = input.uri;
            } else if (input instanceof vscode_1.TabInputNotebook) {
              uri = input.uri;
            }
            if (uri !== void 0 && !seen.has(uri.toString())) {
              seen.add(uri.toString());
              uris !== void 0 && uris.add(uri);
            }
          }
        }
        for (const editor of vscode_1.window.visibleTextEditors) {
          const uri = editor.document.uri;
          if (!seen.has(uri.toString())) {
            seen.add(uri.toString());
            uris !== void 0 && uris.add(uri);
          }
        }
      }
    };
    var BaseLanguageClient = class _BaseLanguageClient {
      _id;
      _name;
      _clientOptions;
      _state;
      _onStart;
      _onStop;
      _connection;
      _idleInterval;
      _ignoredRegistrations;
      // private _idleStart: number | undefined;
      _listeners;
      _disposed;
      _notificationHandlers;
      _notificationDisposables;
      _pendingNotificationHandlers;
      _requestHandlers;
      _requestDisposables;
      _pendingRequestHandlers;
      _progressHandlers;
      _pendingProgressHandlers;
      _progressDisposables;
      _initializeResult;
      _outputChannel;
      _disposeOutputChannel;
      _traceOutputChannel;
      _traceLogLevel;
      _capabilities;
      // undefined: allow lazy creation of the diagnostic collection.
      // null: the diagnostic collection has been disposed and should not be
      // recreated.
      _diagnostics;
      _syncedDocuments;
      _didChangeTextDocumentFeature;
      _inFlightOpenNotifications;
      _pendingChangeSemaphore;
      _pendingChangeDelayer;
      _didOpenTextDocumentFeature;
      _fileEvents;
      _fileEventDelayer;
      _telemetryEmitter;
      _stateChangeEmitter;
      _trace;
      _traceFormat = vscode_languageserver_protocol_1.TraceFormat.Text;
      _tracer;
      _c2p;
      _p2c;
      _visibleDocuments;
      constructor(id, name, clientOptions) {
        this._id = id;
        this._name = name;
        clientOptions = clientOptions || {};
        const markdown = {
          isTrusted: false,
          supportHtml: false,
          supportThemeIcons: false
        };
        if (clientOptions.markdown !== void 0) {
          markdown.isTrusted = ResolvedClientOptions.sanitizeIsTrusted(clientOptions.markdown.isTrusted);
          markdown.supportHtml = clientOptions.markdown.supportHtml === true;
          markdown.supportThemeIcons = clientOptions.markdown.supportThemeIcons === true;
        }
        this._clientOptions = {
          documentSelector: clientOptions.documentSelector ?? [],
          synchronize: clientOptions.synchronize ?? {},
          diagnosticCollectionName: clientOptions.diagnosticCollectionName,
          outputChannelName: clientOptions.outputChannelName ?? this._name,
          revealOutputChannelOn: clientOptions.revealOutputChannelOn ?? RevealOutputChannelOn.Error,
          stdioEncoding: clientOptions.stdioEncoding ?? "utf8",
          initializationOptions: clientOptions.initializationOptions,
          initializationFailedHandler: clientOptions.initializationFailedHandler,
          progressOnInitialization: !!clientOptions.progressOnInitialization,
          errorHandler: clientOptions.errorHandler ?? this.createDefaultErrorHandler(clientOptions.connectionOptions?.maxRestartCount),
          middleware: clientOptions.middleware ?? {},
          uriConverters: clientOptions.uriConverters,
          workspaceFolder: clientOptions.workspaceFolder,
          connectionOptions: clientOptions.connectionOptions,
          markdown,
          // suspend: {
          // 	mode: clientOptions.suspend?.mode ?? SuspendMode.off,
          // 	callback: clientOptions.suspend?.callback ?? (() => Promise.resolve(true)),
          // 	interval: clientOptions.suspend?.interval ? Math.max(clientOptions.suspend.interval, defaultInterval) : defaultInterval
          // },
          diagnosticPullOptions: clientOptions.diagnosticPullOptions ?? { onChange: true, onSave: false },
          diagnosticCollectionProvider: clientOptions.diagnosticCollectionProvider ?? new features_1.DefaultDiagnosticCollectionProvider(),
          notebookDocumentOptions: clientOptions.notebookDocumentOptions ?? {},
          textSynchronization: this.createTextSynchronizationOptions(clientOptions.textSynchronization)
        };
        this._clientOptions.synchronize = this._clientOptions.synchronize || {};
        this._state = ClientState.Initial;
        this._ignoredRegistrations = /* @__PURE__ */ new Set();
        this._listeners = [];
        this._notificationHandlers = /* @__PURE__ */ new Map();
        this._pendingNotificationHandlers = /* @__PURE__ */ new Map();
        this._notificationDisposables = /* @__PURE__ */ new Map();
        this._requestHandlers = /* @__PURE__ */ new Map();
        this._pendingRequestHandlers = /* @__PURE__ */ new Map();
        this._requestDisposables = /* @__PURE__ */ new Map();
        this._progressHandlers = /* @__PURE__ */ new Map();
        this._pendingProgressHandlers = /* @__PURE__ */ new Map();
        this._progressDisposables = /* @__PURE__ */ new Map();
        this._connection = void 0;
        this._initializeResult = void 0;
        if (clientOptions.outputChannel) {
          this._outputChannel = clientOptions.outputChannel;
          this._disposeOutputChannel = false;
          this._traceLogLevel = this._outputChannel.logLevel;
        } else {
          this._outputChannel = void 0;
          this._disposeOutputChannel = true;
          this._traceLogLevel = vscode_1.LogLevel.Info;
        }
        this._traceOutputChannel = clientOptions.traceOutputChannel;
        if (this._traceOutputChannel !== void 0) {
          this._traceLogLevel = this._traceOutputChannel.logLevel;
        }
        this._diagnostics = void 0;
        this._inFlightOpenNotifications = /* @__PURE__ */ new Set();
        this._pendingChangeSemaphore = new async_1.Semaphore(1);
        this._pendingChangeDelayer = new async_1.Delayer(250);
        this._fileEvents = [];
        this._fileEventDelayer = new async_1.Delayer(250);
        this._onStop = void 0;
        this._telemetryEmitter = new vscode_languageserver_protocol_1.Emitter();
        this._stateChangeEmitter = new vscode_languageserver_protocol_1.Emitter();
        this._trace = vscode_languageserver_protocol_1.Trace.Off;
        this._tracer = {
          log: (messageOrDataObject, data) => {
            if (Is2.string(messageOrDataObject)) {
              this.trace(messageOrDataObject, data);
            } else {
              this.traceObject(messageOrDataObject);
            }
          }
        };
        this._c2p = c2p.createConverter(clientOptions.uriConverters ? clientOptions.uriConverters.code2Protocol : void 0);
        this._p2c = p2c.createConverter(clientOptions.uriConverters ? clientOptions.uriConverters.protocol2Code : void 0, this._clientOptions.markdown.isTrusted, this._clientOptions.markdown.supportHtml, this._clientOptions.markdown.supportThemeIcons);
        this._syncedDocuments = /* @__PURE__ */ new Map();
        this.registerBuiltinFeatures();
      }
      createTextSynchronizationOptions(options) {
        if (!options) {
          return { delayOpenNotifications: false };
        }
        if (typeof options.delayOpenNotifications === "boolean") {
          return { delayOpenNotifications: options.delayOpenNotifications };
        }
        return { delayOpenNotifications: false };
      }
      get name() {
        return this._name;
      }
      get middleware() {
        return this._clientOptions.middleware ?? /* @__PURE__ */ Object.create(null);
      }
      get clientOptions() {
        return this._clientOptions;
      }
      get protocol2CodeConverter() {
        return this._p2c;
      }
      get code2ProtocolConverter() {
        return this._c2p;
      }
      get visibleDocuments() {
        if (this._visibleDocuments === void 0) {
          this._visibleDocuments = new VisibleDocumentsImpl();
        }
        return this._visibleDocuments;
      }
      get onTelemetry() {
        return this._telemetryEmitter.event;
      }
      get onDidChangeState() {
        return this._stateChangeEmitter.event;
      }
      get outputChannel() {
        if (!this._outputChannel) {
          this._outputChannel = vscode_1.window.createOutputChannel(this._clientOptions.outputChannelName ? this._clientOptions.outputChannelName : this._name, { log: true });
          if (this._traceOutputChannel === void 0) {
            this._traceLogLevel = this._outputChannel.logLevel;
          }
        }
        return this._outputChannel;
      }
      get traceOutputChannel() {
        return this._traceOutputChannel ? this._traceOutputChannel : this.outputChannel;
      }
      get diagnostics() {
        if (this._diagnostics === null) {
          return void 0;
        }
        if (this._diagnostics === void 0) {
          this._diagnostics = this._clientOptions.diagnosticCollectionProvider.create(this._clientOptions.diagnosticCollectionName ?? this._id, features_1.DiagnosticCollectionSource.push);
        }
        return this._diagnostics;
      }
      get state() {
        return this.getPublicState();
      }
      get $state() {
        return this._state;
      }
      set $state(value) {
        const oldState = this.getPublicState();
        this._state = value;
        const newState = this.getPublicState();
        if (newState !== oldState) {
          this._stateChangeEmitter.fire({ oldState, newState });
        }
      }
      getPublicState() {
        switch (this.$state) {
          case ClientState.Starting:
            return State2.Starting;
          case ClientState.Running:
            return State2.Running;
          case ClientState.StartFailed:
            return State2.StartFailed;
          default:
            return State2.Stopped;
        }
      }
      get initializeResult() {
        return this._initializeResult;
      }
      async sendRequest(type, ...params) {
        if (this.$state === ClientState.StartFailed || this.$state === ClientState.Stopping || this.$state === ClientState.Stopped) {
          return Promise.reject(new vscode_languageserver_protocol_1.ResponseError(vscode_languageserver_protocol_1.ErrorCodes.ConnectionInactive, `Client is not running`));
        }
        const connection = await this.$start();
        await this._didOpenTextDocumentFeature.sendPendingOpenNotifications();
        if (this._didChangeTextDocumentFeature.syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Full) {
          await this.sendPendingFullTextDocumentChanges(connection);
        }
        let param = void 0;
        let token = void 0;
        if (params.length === 1) {
          if (vscode_languageserver_protocol_1.CancellationToken.is(params[0])) {
            token = params[0];
          } else {
            param = params[0];
          }
        } else if (params.length === 2) {
          param = params[0];
          token = params[1];
        }
        if (token !== void 0 && token.isCancellationRequested) {
          return Promise.reject(new vscode_languageserver_protocol_1.ResponseError(vscode_languageserver_protocol_1.LSPErrorCodes.RequestCancelled, "Request got cancelled"));
        }
        const _sendRequest = this._clientOptions.middleware?.sendRequest;
        if (_sendRequest !== void 0) {
          return _sendRequest(type, param, token, (type2, param2, token2) => {
            const params2 = [];
            if (param2 !== void 0) {
              params2.push(param2);
            }
            if (token2 !== void 0) {
              params2.push(token2);
            }
            return connection.sendRequest(type2, ...params2);
          });
        } else {
          return connection.sendRequest(type, ...params);
        }
      }
      onRequest(type, handler) {
        const method = typeof type === "string" ? type : type.method;
        this._requestHandlers.set(method, handler);
        const connection = this.activeConnection();
        let disposable;
        if (connection !== void 0) {
          this._requestDisposables.set(method, connection.onRequest(type, handler));
          disposable = {
            dispose: () => {
              const disposable2 = this._requestDisposables.get(method);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._requestDisposables.delete(method);
              }
            }
          };
        } else {
          this._pendingRequestHandlers.set(method, handler);
          disposable = {
            dispose: () => {
              this._pendingRequestHandlers.delete(method);
              const disposable2 = this._requestDisposables.get(method);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._requestDisposables.delete(method);
              }
            }
          };
        }
        return {
          dispose: () => {
            this._requestHandlers.delete(method);
            disposable.dispose();
          }
        };
      }
      async sendNotification(type, params) {
        if (this.$state === ClientState.StartFailed || this.$state === ClientState.Stopping || this.$state === ClientState.Stopped) {
          return Promise.reject(new vscode_languageserver_protocol_1.ResponseError(vscode_languageserver_protocol_1.ErrorCodes.ConnectionInactive, `Client is not running`));
        }
        const needsPendingFullTextDocumentSync = this._didChangeTextDocumentFeature.syncKind === vscode_languageserver_protocol_1.TextDocumentSyncKind.Full;
        let openNotification;
        if (needsPendingFullTextDocumentSync && typeof type !== "string" && type.method === vscode_languageserver_protocol_1.DidOpenTextDocumentNotification.method) {
          openNotification = params?.textDocument.uri;
          this._inFlightOpenNotifications.add(openNotification);
        }
        let documentToClose;
        if (typeof type !== "string" && type.method === vscode_languageserver_protocol_1.DidCloseTextDocumentNotification.method) {
          documentToClose = params.textDocument.uri;
        }
        const connection = await this.$start();
        const didDropOpenNotification = await this._didOpenTextDocumentFeature.sendPendingOpenNotifications(documentToClose);
        if (didDropOpenNotification) {
          return;
        }
        if (needsPendingFullTextDocumentSync) {
          await this.sendPendingFullTextDocumentChanges(connection);
        }
        if (openNotification !== void 0) {
          this._inFlightOpenNotifications.delete(openNotification);
        }
        const _sendNotification = this._clientOptions.middleware?.sendNotification;
        return _sendNotification ? _sendNotification(type, connection.sendNotification.bind(connection), params) : connection.sendNotification(type, params);
      }
      onNotification(type, handler) {
        const method = typeof type === "string" ? type : type.method;
        this._notificationHandlers.set(method, handler);
        const connection = this.activeConnection();
        let disposable;
        if (connection !== void 0) {
          this._notificationDisposables.set(method, connection.onNotification(type, handler));
          disposable = {
            dispose: () => {
              const disposable2 = this._notificationDisposables.get(method);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._notificationDisposables.delete(method);
              }
            }
          };
        } else {
          this._pendingNotificationHandlers.set(method, handler);
          disposable = {
            dispose: () => {
              this._pendingNotificationHandlers.delete(method);
              const disposable2 = this._notificationDisposables.get(method);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._notificationDisposables.delete(method);
              }
            }
          };
        }
        return {
          dispose: () => {
            this._notificationHandlers.delete(method);
            disposable.dispose();
          }
        };
      }
      async sendProgress(type, token, value) {
        if (this.$state === ClientState.StartFailed || this.$state === ClientState.Stopping || this.$state === ClientState.Stopped) {
          return Promise.reject(new vscode_languageserver_protocol_1.ResponseError(vscode_languageserver_protocol_1.ErrorCodes.ConnectionInactive, `Client is not running`));
        }
        try {
          const connection = await this.$start();
          return connection.sendProgress(type, token, value);
        } catch (error) {
          this.error(`Sending progress for token ${token} failed.`, error);
          throw error;
        }
      }
      onProgress(type, token, handler) {
        this._progressHandlers.set(token, { type, handler });
        const connection = this.activeConnection();
        let disposable;
        const handleWorkDoneProgress = this._clientOptions.middleware?.handleWorkDoneProgress;
        const realHandler = vscode_languageserver_protocol_1.WorkDoneProgress.is(type) && handleWorkDoneProgress !== void 0 ? (params) => {
          handleWorkDoneProgress(token, params, () => handler(params));
        } : handler;
        if (connection !== void 0) {
          this._progressDisposables.set(token, connection.onProgress(type, token, realHandler));
          disposable = {
            dispose: () => {
              const disposable2 = this._progressDisposables.get(token);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._progressDisposables.delete(token);
              }
            }
          };
        } else {
          this._pendingProgressHandlers.set(token, { type, handler });
          disposable = {
            dispose: () => {
              this._pendingProgressHandlers.delete(token);
              const disposable2 = this._progressDisposables.get(token);
              if (disposable2 !== void 0) {
                disposable2.dispose();
                this._progressDisposables.delete(token);
              }
            }
          };
        }
        return {
          dispose: () => {
            this._progressHandlers.delete(token);
            disposable.dispose();
          }
        };
      }
      createDefaultErrorHandler(maxRestartCount) {
        if (maxRestartCount !== void 0 && maxRestartCount < 0) {
          throw new Error(`Invalid maxRestartCount: ${maxRestartCount}`);
        }
        return new DefaultErrorHandler(this, maxRestartCount ?? 4);
      }
      async setTrace(value) {
        this._trace = value;
        const connection = this.activeConnection();
        if (connection !== void 0) {
          await connection.trace(this._trace, this._tracer, {
            sendNotification: false,
            traceFormat: this._traceFormat
          });
        }
      }
      data2String(data) {
        if (data instanceof vscode_languageserver_protocol_1.ResponseError) {
          const responseError = data;
          return `  Message: ${responseError.message}
  Code: ${responseError.code} ${responseError.data ? "\n" + responseError.data.toString() : ""}`;
        }
        if (data instanceof Error) {
          if (Is2.string(data.stack)) {
            return data.stack;
          }
          return data.message;
        }
        if (Is2.string(data)) {
          return data;
        }
        return data.toString();
      }
      shouldLogToOutputChannel() {
        if (this.$state !== ClientState.Stopped) {
          return true;
        }
        return this._outputChannel !== void 0;
      }
      error(message, data, showNotification = true) {
        if (this.shouldLogToOutputChannel()) {
          this.outputChannel.error(this.getLogMessage(message, data));
        }
        if (showNotification === "force" || showNotification && this._clientOptions.revealOutputChannelOn <= RevealOutputChannelOn.Error) {
          this.showNotificationMessage(vscode_languageserver_protocol_1.MessageType.Error, message, data);
        }
      }
      warn(message, data, showNotification = true) {
        if (this.shouldLogToOutputChannel()) {
          this.outputChannel.warn(this.getLogMessage(message, data));
        }
        if (showNotification && this._clientOptions.revealOutputChannelOn <= RevealOutputChannelOn.Warn) {
          this.showNotificationMessage(vscode_languageserver_protocol_1.MessageType.Warning, message, data);
        }
      }
      info(message, data, showNotification = true) {
        if (this.shouldLogToOutputChannel()) {
          this.outputChannel.info(this.getLogMessage(message, data));
        }
        if (showNotification && this._clientOptions.revealOutputChannelOn <= RevealOutputChannelOn.Info) {
          this.showNotificationMessage(vscode_languageserver_protocol_1.MessageType.Info, message, data);
        }
      }
      debug(message, data, showNotification = true) {
        if (this.shouldLogToOutputChannel()) {
          this.outputChannel.debug(this.getLogMessage(message, data));
        }
        if (showNotification && this._clientOptions.revealOutputChannelOn <= RevealOutputChannelOn.Debug) {
          this.showNotificationMessage(vscode_languageserver_protocol_1.MessageType.Debug, message, data);
        }
      }
      trace(message, data) {
        this.traceOutputChannel.trace(this.getLogMessage(message, data));
      }
      traceObject(data) {
        this.traceOutputChannel.trace(JSON.stringify(data));
      }
      showNotificationMessage(type, message, data) {
        message = message ?? "A request has failed. See the output for more information.";
        if (data) {
          message += "\n" + this.data2String(data);
        }
        const messageFunc = type === vscode_languageserver_protocol_1.MessageType.Error ? vscode_1.window.showErrorMessage : type === vscode_languageserver_protocol_1.MessageType.Warning ? vscode_1.window.showWarningMessage : vscode_1.window.showInformationMessage;
        void messageFunc(message, "Go to output").then((selection) => {
          if (selection !== void 0) {
            this.outputChannel.show(true);
          }
        });
      }
      getLogMessage(message, data) {
        return data !== null && data !== void 0 ? `${message}
${this.data2String(data)}` : message;
      }
      needsStart() {
        return this.$state === ClientState.Initial || this.$state === ClientState.Stopping || this.$state === ClientState.Stopped;
      }
      needsStop() {
        return this.$state === ClientState.Starting || this.$state === ClientState.Running;
      }
      activeConnection() {
        return this.$state === ClientState.Running && this._connection !== void 0 ? this._connection : void 0;
      }
      isRunning() {
        return this.$state === ClientState.Running;
      }
      async start() {
        if (this._disposed === "disposing" || this._disposed === "disposed") {
          throw new Error(`Client got disposed and can't be restarted.`);
        }
        if (this.$state === ClientState.Stopping) {
          throw new Error(`Client is currently stopping. Can only restart a full stopped client`);
        }
        if (this._onStart !== void 0) {
          return this._onStart;
        }
        const [promise, resolve, reject] = this.createOnStartPromise();
        this._onStart = promise;
        this._diagnostics = void 0;
        for (const [method, handler] of this._notificationHandlers) {
          if (!this._pendingNotificationHandlers.has(method)) {
            this._pendingNotificationHandlers.set(method, handler);
          }
        }
        for (const [method, handler] of this._requestHandlers) {
          if (!this._pendingRequestHandlers.has(method)) {
            this._pendingRequestHandlers.set(method, handler);
          }
        }
        for (const [token, data] of this._progressHandlers) {
          if (!this._pendingProgressHandlers.has(token)) {
            this._pendingProgressHandlers.set(token, data);
          }
        }
        this.$state = ClientState.Starting;
        try {
          const connection = await this.createConnection();
          connection.onNotification(vscode_languageserver_protocol_1.LogMessageNotification.type, (message) => {
            switch (message.type) {
              case vscode_languageserver_protocol_1.MessageType.Error:
                this.error(message.message, void 0, false);
                break;
              case vscode_languageserver_protocol_1.MessageType.Warning:
                this.warn(message.message, void 0, false);
                break;
              case vscode_languageserver_protocol_1.MessageType.Info:
                this.info(message.message, void 0, false);
                break;
              case vscode_languageserver_protocol_1.MessageType.Debug:
                this.debug(message.message, void 0, false);
                break;
              default:
                this.outputChannel.appendLine(message.message);
            }
          });
          connection.onNotification(vscode_languageserver_protocol_1.ShowMessageNotification.type, (message) => {
            switch (message.type) {
              case vscode_languageserver_protocol_1.MessageType.Error:
                void vscode_1.window.showErrorMessage(message.message);
                break;
              case vscode_languageserver_protocol_1.MessageType.Warning:
                void vscode_1.window.showWarningMessage(message.message);
                break;
              case vscode_languageserver_protocol_1.MessageType.Info:
                void vscode_1.window.showInformationMessage(message.message);
                break;
              default:
                void vscode_1.window.showInformationMessage(message.message);
            }
          });
          connection.onRequest(vscode_languageserver_protocol_1.ShowMessageRequest.type, (params) => {
            let messageFunc;
            switch (params.type) {
              case vscode_languageserver_protocol_1.MessageType.Error:
                messageFunc = vscode_1.window.showErrorMessage;
                break;
              case vscode_languageserver_protocol_1.MessageType.Warning:
                messageFunc = vscode_1.window.showWarningMessage;
                break;
              case vscode_languageserver_protocol_1.MessageType.Info:
                messageFunc = vscode_1.window.showInformationMessage;
                break;
              default:
                messageFunc = vscode_1.window.showInformationMessage;
            }
            const actions = params.actions || [];
            return messageFunc(params.message, ...actions);
          });
          connection.onNotification(vscode_languageserver_protocol_1.TelemetryEventNotification.type, (data) => {
            this._telemetryEmitter.fire(data);
          });
          connection.onRequest(vscode_languageserver_protocol_1.ShowDocumentRequest.type, async (params, token) => {
            const showDocument = async (params2) => {
              const uri = this.protocol2CodeConverter.asUri(params2.uri);
              try {
                if (params2.external === true) {
                  const success = await vscode_1.env.openExternal(uri);
                  return { success };
                } else {
                  const options = {};
                  if (params2.selection !== void 0) {
                    options.selection = this.protocol2CodeConverter.asRange(params2.selection);
                  }
                  if (params2.takeFocus === void 0 || params2.takeFocus === false) {
                    options.preserveFocus = true;
                  } else if (params2.takeFocus === true) {
                    options.preserveFocus = false;
                  }
                  await vscode_1.window.showTextDocument(uri, options);
                  return { success: true };
                }
              } catch (error) {
                return { success: false };
              }
            };
            const middleware = this._clientOptions.middleware.window?.showDocument;
            if (middleware !== void 0) {
              return middleware(params, token, showDocument);
            } else {
              return showDocument(params);
            }
          });
          connection.listen();
          await this.initialize(connection);
          resolve();
        } catch (error) {
          this.$state = ClientState.StartFailed;
          this.error(`${this._name} client: couldn't create connection to server.`, error, "force");
          reject(error);
        }
        return this._onStart;
      }
      createOnStartPromise() {
        let resolve;
        let reject;
        const promise = new Promise((_resolve, _reject) => {
          resolve = _resolve;
          reject = _reject;
        });
        return [promise, resolve, reject];
      }
      async initialize(connection) {
        this.refreshTrace(connection, false);
        const initOption = this._clientOptions.initializationOptions;
        const [rootPath, workspaceFolders] = this._clientOptions.workspaceFolder !== void 0 ? [this._clientOptions.workspaceFolder.uri.fsPath, [{ uri: this._c2p.asUri(this._clientOptions.workspaceFolder.uri), name: this._clientOptions.workspaceFolder.name }]] : [this._clientGetRootPath(), null];
        const initParams = {
          processId: null,
          clientInfo: {
            name: vscode_1.env.appName,
            version: vscode_1.version
          },
          locale: this.getLocale(),
          rootPath: rootPath ? rootPath : null,
          rootUri: rootPath ? this._c2p.asUri(vscode_1.Uri.file(rootPath)) : null,
          capabilities: this.computeClientCapabilities(),
          initializationOptions: Is2.func(initOption) ? initOption() : initOption,
          trace: vscode_languageserver_protocol_1.Trace.toString(this._trace),
          workspaceFolders
        };
        this.fillInitializeParams(initParams);
        if (this._clientOptions.progressOnInitialization) {
          const token = UUID.generateUuid();
          const part = new progressPart_1.ProgressPart(connection, token);
          initParams.workDoneToken = token;
          try {
            const result = await this.doInitialize(connection, initParams);
            part.done();
            return result;
          } catch (error) {
            part.cancel();
            throw error;
          }
        } else {
          return this.doInitialize(connection, initParams);
        }
      }
      async doInitialize(connection, initParams) {
        try {
          const result = await connection.initialize(initParams);
          if (result.capabilities.positionEncoding !== void 0 && result.capabilities.positionEncoding !== vscode_languageserver_protocol_1.PositionEncodingKind.UTF16) {
            throw new Error(`Unsupported position encoding (${result.capabilities.positionEncoding}) received from server ${this.name}`);
          }
          this._initializeResult = result;
          this.$state = ClientState.Running;
          let textDocumentSyncOptions = void 0;
          if (Is2.number(result.capabilities.textDocumentSync)) {
            if (result.capabilities.textDocumentSync === vscode_languageserver_protocol_1.TextDocumentSyncKind.None) {
              textDocumentSyncOptions = {
                openClose: false,
                change: vscode_languageserver_protocol_1.TextDocumentSyncKind.None,
                save: void 0
              };
            } else {
              textDocumentSyncOptions = {
                openClose: true,
                change: result.capabilities.textDocumentSync,
                save: {
                  includeText: false
                }
              };
            }
          } else if (result.capabilities.textDocumentSync !== void 0 && result.capabilities.textDocumentSync !== null) {
            textDocumentSyncOptions = result.capabilities.textDocumentSync;
          }
          this._capabilities = Object.assign({}, result.capabilities, { resolvedTextDocumentSync: textDocumentSyncOptions });
          connection.onNotification(vscode_languageserver_protocol_1.PublishDiagnosticsNotification.type, (params) => this.handleDiagnostics(params));
          connection.onRequest(vscode_languageserver_protocol_1.RegistrationRequest.type, (params) => this.handleRegistrationRequest(params));
          connection.onRequest("client/registerFeature", (params) => this.handleRegistrationRequest(params));
          connection.onRequest(vscode_languageserver_protocol_1.UnregistrationRequest.type, (params) => this.handleUnregistrationRequest(params));
          connection.onRequest("client/unregisterFeature", (params) => this.handleUnregistrationRequest(params));
          connection.onRequest(vscode_languageserver_protocol_1.ApplyWorkspaceEditRequest.type, (params) => this.handleApplyWorkspaceEdit(params));
          for (const [method, handler] of this._pendingNotificationHandlers) {
            this._notificationDisposables.set(method, connection.onNotification(method, handler));
          }
          this._pendingNotificationHandlers.clear();
          for (const [method, handler] of this._pendingRequestHandlers) {
            this._requestDisposables.set(method, connection.onRequest(method, handler));
          }
          this._pendingRequestHandlers.clear();
          for (const [token, data] of this._pendingProgressHandlers) {
            this._progressDisposables.set(token, connection.onProgress(data.type, token, data.handler));
          }
          this._pendingProgressHandlers.clear();
          await connection.sendNotification(vscode_languageserver_protocol_1.InitializedNotification.type, {});
          this.hookFileEvents(connection);
          this.hookLogLevelChanged(connection);
          this.hookConfigurationChanged(connection);
          this.initializeFeatures(connection);
          return result;
        } catch (error) {
          if (this._clientOptions.initializationFailedHandler) {
            if (this._clientOptions.initializationFailedHandler(error)) {
              void this.initialize(connection);
            } else {
              void this.stop();
            }
          } else if (error instanceof vscode_languageserver_protocol_1.ResponseError && error.data && error.data.retry) {
            void vscode_1.window.showErrorMessage(error.message, { title: "Retry", id: "retry" }).then((item) => {
              if (item && item.id === "retry") {
                void this.initialize(connection);
              } else {
                void this.stop();
              }
            });
          } else {
            if (error && error.message) {
              void vscode_1.window.showErrorMessage(error.message);
            }
            this.error("Server initialization failed.", error);
            void this.stop();
          }
          throw error;
        }
      }
      _clientGetRootPath() {
        const folders = vscode_1.workspace.workspaceFolders;
        if (!folders || folders.length === 0) {
          return void 0;
        }
        const folder = folders[0];
        if (folder.uri.scheme === "file") {
          return folder.uri.fsPath;
        }
        return void 0;
      }
      stop(timeout = 2e3) {
        return this.shutdown(ShutdownMode.Stop, timeout);
      }
      dispose(timeout = 2e3) {
        try {
          this._disposed = "disposing";
          return this.stop(timeout);
        } finally {
          this._disposed = "disposed";
        }
      }
      async shutdown(mode, timeout = 2e3) {
        if (this.$state === ClientState.Stopped || this.$state === ClientState.Initial) {
          return;
        }
        if (this.$state === ClientState.Stopping) {
          if (this._onStop !== void 0) {
            return this._onStop;
          } else {
            throw new Error(`Client is stopping but no stop promise available.`);
          }
        }
        const connection = this.activeConnection();
        if (connection === void 0 || this.$state !== ClientState.Running) {
          throw new Error(`Client is not running and can't be stopped. It's current state is: ${this.$state}`);
        }
        this._initializeResult = void 0;
        this.$state = ClientState.Stopping;
        this.cleanUp(mode);
        const tp = new Promise((c) => {
          (0, vscode_languageserver_protocol_1.RAL)().timer.setTimeout(c, timeout);
        });
        const shutdown = (async (connection2) => {
          await connection2.shutdown();
          await connection2.exit();
          return connection2;
        })(connection);
        return this._onStop = Promise.race([tp, shutdown]).then((connection2) => {
          if (connection2 !== void 0) {
            connection2.end();
            connection2.dispose();
          } else {
            this.error(`Stopping server timed out`, void 0, false);
            throw new Error(`Stopping the server timed out`);
          }
        }, (error) => {
          this.error(`Stopping server failed`, error, false);
          throw error;
        }).finally(() => {
          this.$state = ClientState.Stopped;
          mode === ShutdownMode.Stop && this.cleanUpChannel();
          this._onStart = void 0;
          this._onStop = void 0;
          this._connection = void 0;
          this._ignoredRegistrations.clear();
        });
      }
      cleanUp(mode) {
        this._fileEvents = [];
        this._fileEventDelayer.cancel();
        const disposables = this._listeners.splice(0, this._listeners.length);
        for (const disposable of disposables) {
          disposable.dispose();
        }
        if (this._syncedDocuments) {
          this._syncedDocuments.clear();
        }
        for (const feature of Array.from(this._features.entries()).map((entry) => entry[1]).reverse()) {
          feature.clear();
        }
        if (mode === ShutdownMode.Stop || mode === ShutdownMode.Restart) {
          if (this._diagnostics === void 0) {
            this._diagnostics = null;
          }
          if (this._diagnostics !== null) {
            this._clientOptions.diagnosticCollectionProvider.dispose(this._diagnostics, features_1.DiagnosticCollectionSource.push);
            this._diagnostics = null;
          }
        }
        if (this._idleInterval !== void 0) {
          this._idleInterval.dispose();
          this._idleInterval = void 0;
        }
      }
      cleanUpChannel() {
        if (this._outputChannel !== void 0 && this._disposeOutputChannel) {
          this._outputChannel.dispose();
          this._outputChannel = void 0;
        }
      }
      notifyFileEvent(event) {
        const client = this;
        async function didChangeWatchedFile(event2) {
          client._fileEvents.push(event2);
          return client._fileEventDelayer.trigger(async () => {
            const fileEvents = client._fileEvents;
            client._fileEvents = [];
            try {
              await client.sendNotification(vscode_languageserver_protocol_1.DidChangeWatchedFilesNotification.type, { changes: fileEvents });
            } catch (error) {
              client._fileEvents.push(...fileEvents);
              throw error;
            }
          });
        }
        const workSpaceMiddleware = this.clientOptions.middleware?.workspace;
        (workSpaceMiddleware?.didChangeWatchedFile ? workSpaceMiddleware.didChangeWatchedFile(event, didChangeWatchedFile) : didChangeWatchedFile(event)).catch((error) => {
          client.error(`Notifying file events failed.`, error);
        });
      }
      async sendPendingFullTextDocumentChanges(connection) {
        return this._pendingChangeSemaphore.lock(async () => {
          try {
            const changes = this._didChangeTextDocumentFeature.getPendingDocumentChanges(this._inFlightOpenNotifications);
            if (changes.length === 0) {
              return;
            }
            for (const document of changes) {
              const params = this.code2ProtocolConverter.asChangeTextDocumentParams(document);
              this._didChangeTextDocumentFeature.aboutToSendNotification(document, vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
              await connection.sendNotification(vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
              this._didChangeTextDocumentFeature.notificationSent(document, vscode_languageserver_protocol_1.DidChangeTextDocumentNotification.type, params);
            }
          } catch (error) {
            this.error(`Sending pending changes failed`, error, false);
            throw error;
          }
        });
      }
      triggerPendingChangeDelivery() {
        this._pendingChangeDelayer.trigger(async () => {
          const connection = this.activeConnection();
          if (connection === void 0) {
            this.triggerPendingChangeDelivery();
            return;
          }
          await this.sendPendingFullTextDocumentChanges(connection);
        }).catch((error) => this.error(`Delivering pending changes failed`, error, false));
      }
      _diagnosticQueue = /* @__PURE__ */ new Map();
      _diagnosticQueueState = { state: "idle" };
      handleDiagnostics(params) {
        if (this._diagnostics === null) {
          return;
        }
        const key = params.uri;
        if (this._diagnosticQueueState.state === "busy" && this._diagnosticQueueState.document === key) {
          this._diagnosticQueueState.tokenSource.cancel();
        }
        this._diagnosticQueue.set(params.uri, params.diagnostics);
        this.triggerDiagnosticQueue();
      }
      triggerDiagnosticQueue() {
        (0, vscode_languageserver_protocol_1.RAL)().timer.setImmediate(() => {
          this.workDiagnosticQueue();
        });
      }
      workDiagnosticQueue() {
        if (this._diagnosticQueueState.state === "busy") {
          return;
        }
        const next = this._diagnosticQueue.entries().next();
        if (next.done === true) {
          return;
        }
        const [document, diagnostics] = next.value;
        this._diagnosticQueue.delete(document);
        const tokenSource = new vscode_1.CancellationTokenSource();
        this._diagnosticQueueState = { state: "busy", document, tokenSource };
        this._p2c.asDiagnostics(diagnostics, tokenSource.token).then((converted) => {
          if (!tokenSource.token.isCancellationRequested) {
            const uri = this._p2c.asUri(document);
            const middleware = this.clientOptions.middleware;
            if (middleware.handleDiagnostics) {
              middleware.handleDiagnostics(uri, converted, (uri2, diagnostics2) => this.setDiagnostics(uri2, diagnostics2));
            } else {
              this.setDiagnostics(uri, converted);
            }
          }
        }).catch((error) => {
          this.error(`Processing diagnostic queue failed.`, error);
        }).finally(() => {
          this._diagnosticQueueState = { state: "idle" };
          this.triggerDiagnosticQueue();
        });
      }
      setDiagnostics(uri, diagnostics) {
        if (this._diagnostics === null) {
          return;
        }
        const diagnosticsCollection = this.diagnostics;
        if (diagnosticsCollection !== void 0) {
          diagnosticsCollection.set(uri, diagnostics);
        }
      }
      getLocale() {
        return vscode_1.env.language;
      }
      async $start() {
        if (this.$state === ClientState.StartFailed) {
          throw new Error(`Previous start failed. Can't restart server.`);
        }
        await this.start();
        const connection = this.activeConnection();
        if (connection === void 0) {
          throw new Error(`Starting server failed`);
        }
        return connection;
      }
      async createConnection() {
        const errorHandler = (error, message, count) => {
          this.handleConnectionError(error, message, count).catch((error2) => this.error(`Handling connection error failed`, error2));
        };
        const closeHandler = () => {
          this.handleConnectionClosed().catch((error) => this.error(`Handling connection close failed`, error));
        };
        const transports = await this.createMessageTransports(this._clientOptions.stdioEncoding || "utf8");
        this._connection = createConnection(transports.reader, transports.writer, errorHandler, closeHandler, this._clientOptions.connectionOptions);
        return this._connection;
      }
      async handleConnectionClosed() {
        if (this.$state === ClientState.Stopped) {
          return;
        }
        try {
          if (this._connection !== void 0) {
            this._connection.dispose();
          }
        } catch (error) {
        }
        let handlerResult = { action: CloseAction.DoNotRestart };
        if (this.$state !== ClientState.Stopping) {
          try {
            handlerResult = await this._clientOptions.errorHandler.closed();
          } catch (error) {
          }
        }
        this._connection = void 0;
        if (handlerResult.action === CloseAction.DoNotRestart) {
          this.error(handlerResult.message ?? "Connection to server got closed. Server will not be restarted.", void 0, handlerResult.handled === true ? false : "force");
          this.cleanUp(ShutdownMode.Stop);
          if (this.$state === ClientState.Starting) {
            this.$state = ClientState.StartFailed;
          } else {
            this.$state = ClientState.Stopped;
          }
          this._onStop = Promise.resolve();
          this._onStart = void 0;
        } else if (handlerResult.action === CloseAction.Restart) {
          this.info(handlerResult.message ?? "Connection to server got closed. Server will restart.", void 0, !handlerResult.handled);
          this.cleanUp(ShutdownMode.Restart);
          this.$state = ClientState.Initial;
          this._onStop = Promise.resolve();
          this._onStart = void 0;
          this.start().catch((error) => this.error(`Restarting server failed`, error, "force"));
        }
      }
      async handleConnectionError(error, message, count) {
        const handlerResult = await this._clientOptions.errorHandler.error(error, message, count);
        if (handlerResult.action === ErrorAction.Shutdown) {
          this.error(handlerResult.message ?? `Client ${this._name}: connection to server is erroring.
${error.message}
Shutting down server.`, void 0, handlerResult.handled === true ? false : "force");
          this.stop().catch((error2) => {
            this.error(`Stopping server failed`, error2, false);
          });
        } else {
          this.error(handlerResult.message ?? `Client ${this._name}: connection to server is erroring.
${error.message}`, void 0, handlerResult.handled === true ? false : "force");
        }
      }
      hookConfigurationChanged(connection) {
        this._listeners.push(vscode_1.workspace.onDidChangeConfiguration(() => {
          this.refreshTrace(connection, true);
        }));
      }
      hookLogLevelChanged(connection) {
        this._listeners.push(this.traceOutputChannel.onDidChangeLogLevel((level) => {
          this._traceLogLevel = level;
          this.refreshTrace(connection, true);
        }));
      }
      refreshTrace(connection, sendNotification = false) {
        const config = vscode_1.workspace.getConfiguration(this._id);
        let trace = this._traceLogLevel !== vscode_1.LogLevel.Trace ? vscode_languageserver_protocol_1.Trace.Off : vscode_languageserver_protocol_1.Trace.Messages;
        let traceFormat = vscode_languageserver_protocol_1.TraceFormat.Text;
        if (config && trace !== vscode_languageserver_protocol_1.Trace.Off) {
          const traceConfig = config.get("trace.server", "messages");
          if (typeof traceConfig === "string") {
            trace = vscode_languageserver_protocol_1.Trace.fromString(traceConfig);
            if (trace === vscode_languageserver_protocol_1.Trace.Off) {
              trace = vscode_languageserver_protocol_1.Trace.Messages;
            }
          } else {
            trace = vscode_languageserver_protocol_1.Trace.fromString(config.get("trace.server.verbosity", "messages"));
            if (trace === vscode_languageserver_protocol_1.Trace.Off) {
              trace = vscode_languageserver_protocol_1.Trace.Messages;
            }
            traceFormat = vscode_languageserver_protocol_1.TraceFormat.fromString(config.get("trace.server.format", "text"));
          }
        }
        this._trace = trace;
        this._traceFormat = traceFormat;
        connection.trace(this._trace, this._tracer, {
          sendNotification,
          traceFormat: this._traceFormat
        }).catch((error) => {
          this.error(`Updating trace failed with error`, error, false);
        });
      }
      hookFileEvents(_connection) {
        const fileEvents = this._clientOptions.synchronize.fileEvents;
        if (!fileEvents) {
          return;
        }
        let watchers;
        if (Is2.array(fileEvents)) {
          watchers = fileEvents;
        } else {
          watchers = [fileEvents];
        }
        if (!watchers) {
          return;
        }
        this._dynamicFeatures.get(vscode_languageserver_protocol_1.DidChangeWatchedFilesNotification.type.method).registerRaw(UUID.generateUuid(), watchers);
      }
      _features = [];
      _dynamicFeatures = /* @__PURE__ */ new Map();
      registerFeatures(features) {
        for (const feature of features) {
          this.registerFeature(feature);
        }
      }
      registerFeature(feature) {
        this._features.push(feature);
        if (features_1.DynamicFeature.is(feature)) {
          const registrationType = feature.registrationType;
          this._dynamicFeatures.set(registrationType.method, feature);
        }
      }
      getFeature(request) {
        return this._dynamicFeatures.get(request);
      }
      hasDedicatedTextSynchronizationFeature(textDocument) {
        const feature = this.getFeature(vscode_languageserver_protocol_1.NotebookDocumentSyncRegistrationType.method);
        if (feature === void 0 || !(feature instanceof notebook_1.NotebookDocumentSyncFeature)) {
          return false;
        }
        return feature.handles(textDocument);
      }
      registerBuiltinFeatures() {
        const pendingFullTextDocumentChanges = /* @__PURE__ */ new Map();
        this.registerFeature(new configuration_1.ConfigurationFeature(this));
        this._didOpenTextDocumentFeature = new textSynchronization_1.DidOpenTextDocumentFeature(this, this._syncedDocuments);
        this.registerFeature(this._didOpenTextDocumentFeature);
        this._didChangeTextDocumentFeature = new textSynchronization_1.DidChangeTextDocumentFeature(this, pendingFullTextDocumentChanges);
        this._didChangeTextDocumentFeature.onPendingChangeAdded(() => {
          this.triggerPendingChangeDelivery();
        });
        this.registerFeature(this._didChangeTextDocumentFeature);
        this.registerFeature(new textSynchronization_1.WillSaveFeature(this));
        this.registerFeature(new textSynchronization_1.WillSaveWaitUntilFeature(this));
        this.registerFeature(new textSynchronization_1.DidSaveTextDocumentFeature(this));
        this.registerFeature(new textSynchronization_1.DidCloseTextDocumentFeature(this, this._syncedDocuments, pendingFullTextDocumentChanges));
        this.registerFeature(new fileSystemWatcher_1.FileSystemWatcherFeature(this, (event) => this.notifyFileEvent(event)));
        this.registerFeature(new completion_1.CompletionItemFeature(this));
        this.registerFeature(new hover_1.HoverFeature(this));
        this.registerFeature(new signatureHelp_1.SignatureHelpFeature(this));
        this.registerFeature(new definition_1.DefinitionFeature(this));
        this.registerFeature(new reference_1.ReferencesFeature(this));
        this.registerFeature(new documentHighlight_1.DocumentHighlightFeature(this));
        this.registerFeature(new documentSymbol_1.DocumentSymbolFeature(this));
        this.registerFeature(new workspaceSymbol_1.WorkspaceSymbolFeature(this));
        this.registerFeature(new codeAction_1.CodeActionFeature(this));
        this.registerFeature(new codeLens_1.CodeLensFeature(this));
        this.registerFeature(new formatting_1.DocumentFormattingFeature(this));
        this.registerFeature(new formatting_1.DocumentRangeFormattingFeature(this));
        this.registerFeature(new formatting_1.DocumentOnTypeFormattingFeature(this));
        this.registerFeature(new rename_1.RenameFeature(this));
        this.registerFeature(new documentLink_1.DocumentLinkFeature(this));
        this.registerFeature(new executeCommand_1.ExecuteCommandFeature(this));
        this.registerFeature(new configuration_1.SyncConfigurationFeature(this));
        this.registerFeature(new typeDefinition_1.TypeDefinitionFeature(this));
        this.registerFeature(new implementation_1.ImplementationFeature(this));
        this.registerFeature(new colorProvider_1.ColorProviderFeature(this));
        if (this.clientOptions.workspaceFolder === void 0) {
          this.registerFeature(new workspaceFolder_1.WorkspaceFoldersFeature(this));
        }
        this.registerFeature(new foldingRange_1.FoldingRangeFeature(this));
        this.registerFeature(new declaration_1.DeclarationFeature(this));
        this.registerFeature(new selectionRange_1.SelectionRangeFeature(this));
        this.registerFeature(new progress_1.ProgressFeature(this));
        this.registerFeature(new callHierarchy_1.CallHierarchyFeature(this));
        this.registerFeature(new semanticTokens_1.SemanticTokensFeature(this));
        this.registerFeature(new linkedEditingRange_1.LinkedEditingFeature(this));
        this.registerFeature(new fileOperations_1.DidCreateFilesFeature(this));
        this.registerFeature(new fileOperations_1.DidRenameFilesFeature(this));
        this.registerFeature(new fileOperations_1.DidDeleteFilesFeature(this));
        this.registerFeature(new fileOperations_1.WillCreateFilesFeature(this));
        this.registerFeature(new fileOperations_1.WillRenameFilesFeature(this));
        this.registerFeature(new fileOperations_1.WillDeleteFilesFeature(this));
        this.registerFeature(new typeHierarchy_1.TypeHierarchyFeature(this));
        this.registerFeature(new inlineValue_1.InlineValueFeature(this));
        this.registerFeature(new inlayHint_1.InlayHintsFeature(this));
        this.registerFeature(new diagnostic_1.DiagnosticFeature(this));
        this.registerFeature(new notebook_1.NotebookDocumentSyncFeature(this));
        this.registerFeature(new inlineCompletion_1.InlineCompletionItemFeature(this));
        this.registerFeature(new textDocumentContent_1.TextDocumentContentFeature(this));
      }
      registerProposedFeatures() {
        this.registerFeatures(ProposedFeatures.createAll(this));
      }
      fillInitializeParams(params) {
        for (const feature of this._features) {
          if (Is2.func(feature.fillInitializeParams)) {
            feature.fillInitializeParams(params);
          }
        }
      }
      computeClientCapabilities() {
        const result = {};
        (0, features_1.ensure)(result, "workspace").applyEdit = true;
        const workspaceEdit = (0, features_1.ensure)((0, features_1.ensure)(result, "workspace"), "workspaceEdit");
        workspaceEdit.documentChanges = true;
        workspaceEdit.resourceOperations = [vscode_languageserver_protocol_1.ResourceOperationKind.Create, vscode_languageserver_protocol_1.ResourceOperationKind.Rename, vscode_languageserver_protocol_1.ResourceOperationKind.Delete];
        workspaceEdit.failureHandling = vscode_languageserver_protocol_1.FailureHandlingKind.TextOnlyTransactional;
        workspaceEdit.normalizesLineEndings = true;
        workspaceEdit.changeAnnotationSupport = {
          groupsOnLabel: true
        };
        workspaceEdit.metadataSupport = true;
        workspaceEdit.snippetEditSupport = true;
        const diagnostics = (0, features_1.ensure)((0, features_1.ensure)(result, "textDocument"), "publishDiagnostics");
        diagnostics.relatedInformation = true;
        diagnostics.versionSupport = false;
        diagnostics.tagSupport = { valueSet: [vscode_languageserver_protocol_1.DiagnosticTag.Unnecessary, vscode_languageserver_protocol_1.DiagnosticTag.Deprecated] };
        diagnostics.codeDescriptionSupport = true;
        diagnostics.dataSupport = true;
        const textDocumentFilter = (0, features_1.ensure)((0, features_1.ensure)(result, "textDocument"), "filters");
        textDocumentFilter.relativePatternSupport = true;
        const windowCapabilities = (0, features_1.ensure)(result, "window");
        const showMessage = (0, features_1.ensure)(windowCapabilities, "showMessage");
        showMessage.messageActionItem = { additionalPropertiesSupport: true };
        const showDocument = (0, features_1.ensure)(windowCapabilities, "showDocument");
        showDocument.support = true;
        const generalCapabilities = (0, features_1.ensure)(result, "general");
        generalCapabilities.staleRequestSupport = {
          cancel: true,
          retryOnContentModified: Array.from(_BaseLanguageClient.RequestsToCancelOnContentModified)
        };
        generalCapabilities.regularExpressions = { engine: "ECMAScript", version: "ES2020" };
        generalCapabilities.markdown = {
          parser: "marked",
          version: "1.1.0"
        };
        generalCapabilities.positionEncodings = ["utf-16"];
        if (this._clientOptions.markdown.supportHtml) {
          generalCapabilities.markdown.allowedTags = ["ul", "li", "p", "code", "blockquote", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "em", "pre", "table", "thead", "tbody", "tr", "th", "td", "div", "del", "a", "strong", "br", "img", "span"];
        }
        for (const feature of this._features) {
          feature.fillClientCapabilities(result);
        }
        return result;
      }
      initializeFeatures(_connection) {
        const documentSelector = this._clientOptions.documentSelector;
        for (const feature of this._features) {
          if (Is2.func(feature.preInitialize)) {
            feature.preInitialize(this._capabilities, documentSelector);
          }
        }
        for (const feature of this._features) {
          feature.initialize(this._capabilities, documentSelector);
        }
      }
      async handleRegistrationRequest(params) {
        const middleware = this.clientOptions.middleware?.handleRegisterCapability;
        if (middleware) {
          return middleware(params, (nextParams) => this.doRegisterCapability(nextParams));
        } else {
          return this.doRegisterCapability(params);
        }
      }
      async doRegisterCapability(params) {
        if (!this.isRunning()) {
          for (const registration of params.registrations) {
            this._ignoredRegistrations.add(registration.id);
          }
          return;
        }
        for (const registration of params.registrations) {
          const feature = this._dynamicFeatures.get(registration.method);
          if (feature === void 0) {
            return Promise.reject(new Error(`No feature implementation for ${registration.method} found. Registration failed.`));
          }
          const options = registration.registerOptions ?? {};
          options.documentSelector = options.documentSelector ?? this._clientOptions.documentSelector;
          const data = {
            id: registration.id,
            registerOptions: options
          };
          try {
            feature.register(data);
          } catch (err) {
            return Promise.reject(err);
          }
        }
      }
      async handleUnregistrationRequest(params) {
        const middleware = this.clientOptions.middleware?.handleUnregisterCapability;
        if (middleware) {
          return middleware(params, (nextParams) => this.doUnregisterCapability(nextParams));
        } else {
          return this.doUnregisterCapability(params);
        }
      }
      async doUnregisterCapability(params) {
        for (const unregistration of params.unregisterations) {
          if (this._ignoredRegistrations.has(unregistration.id)) {
            continue;
          }
          const feature = this._dynamicFeatures.get(unregistration.method);
          if (!feature) {
            return Promise.reject(new Error(`No feature implementation for ${unregistration.method} found. Unregistration failed.`));
          }
          feature.unregister(unregistration.id);
        }
      }
      async handleApplyWorkspaceEdit(params) {
        const middleware = this.clientOptions.middleware?.workspace?.handleApplyEdit;
        if (middleware) {
          const resultOrError = await middleware(params, (nextParams) => this.doHandleApplyWorkspaceEdit(nextParams));
          if (resultOrError instanceof vscode_languageserver_protocol_1.ResponseError) {
            return Promise.reject(resultOrError);
          }
          return resultOrError;
        } else {
          return this.doHandleApplyWorkspaceEdit(params);
        }
      }
      workspaceEditLock = new async_1.Semaphore(1);
      async doHandleApplyWorkspaceEdit(params) {
        const workspaceEdit = params.edit;
        const converted = await this.workspaceEditLock.lock(() => {
          return this._p2c.asWorkspaceEdit(workspaceEdit);
        });
        const valid = this.validateWorkspaceEdit(workspaceEdit);
        if (!valid) {
          return Promise.resolve({ applied: false });
        }
        return Is2.asPromise(vscode_1.workspace.applyEdit(converted, { isRefactoring: params.metadata?.isRefactoring }).then((value) => {
          return { applied: value };
        }));
      }
      validateWorkspaceEdit(workspaceEdit) {
        const openTextDocuments = /* @__PURE__ */ new Map();
        vscode_1.workspace.textDocuments.forEach((document) => openTextDocuments.set(document.uri.toString(), document));
        if (workspaceEdit.documentChanges) {
          for (const change of workspaceEdit.documentChanges) {
            if (vscode_languageserver_protocol_1.TextDocumentEdit.is(change) && change.textDocument.version !== null && change.textDocument.version >= 0) {
              const changeUri = this._p2c.asUri(change.textDocument.uri).toString();
              const textDocument = openTextDocuments.get(changeUri);
              if (textDocument && textDocument.version !== change.textDocument.version) {
                return false;
              }
            }
          }
        }
        return true;
      }
      static RequestsToCancelOnContentModified = /* @__PURE__ */ new Set([
        vscode_languageserver_protocol_1.SemanticTokensRequest.method,
        vscode_languageserver_protocol_1.SemanticTokensRangeRequest.method,
        vscode_languageserver_protocol_1.SemanticTokensDeltaRequest.method
      ]);
      static CancellableResolveCalls = /* @__PURE__ */ new Set([
        vscode_languageserver_protocol_1.CompletionResolveRequest.method,
        vscode_languageserver_protocol_1.CodeLensResolveRequest.method,
        vscode_languageserver_protocol_1.CodeActionResolveRequest.method,
        vscode_languageserver_protocol_1.InlayHintResolveRequest.method,
        vscode_languageserver_protocol_1.DocumentLinkResolveRequest.method,
        vscode_languageserver_protocol_1.WorkspaceSymbolResolveRequest.method
      ]);
      handleFailedRequest(type, token, error, defaultValue, showNotification = true, throwOnCancel = false) {
        if (error instanceof vscode_languageserver_protocol_1.ResponseError) {
          if (error.code === vscode_languageserver_protocol_1.ErrorCodes.PendingResponseRejected || error.code === vscode_languageserver_protocol_1.ErrorCodes.ConnectionInactive) {
            return defaultValue;
          }
          if (error.code === vscode_languageserver_protocol_1.LSPErrorCodes.RequestCancelled || error.code === vscode_languageserver_protocol_1.LSPErrorCodes.ServerCancelled) {
            if (token !== void 0 && token.isCancellationRequested && !throwOnCancel) {
              return defaultValue;
            } else {
              if (error.data !== void 0) {
                throw new features_1.LSPCancellationError(error.data);
              } else {
                throw new vscode_1.CancellationError();
              }
            }
          } else if (error.code === vscode_languageserver_protocol_1.LSPErrorCodes.ContentModified) {
            if (_BaseLanguageClient.RequestsToCancelOnContentModified.has(type.method) || _BaseLanguageClient.CancellableResolveCalls.has(type.method)) {
              throw new vscode_1.CancellationError();
            } else {
              return defaultValue;
            }
          }
        }
        this.error(`Request ${type.method} failed.`, error, showNotification);
        throw error;
      }
    };
    exports2.BaseLanguageClient = BaseLanguageClient;
    var LanguageClient2 = class extends BaseLanguageClient {
      serverOptions;
      constructor(id, name, serverOptions, clientOptions) {
        super(id, name, clientOptions);
        this.serverOptions = serverOptions;
      }
      async createMessageTransports(_encoding) {
        return this.serverOptions();
      }
    };
    exports2.LanguageClient = LanguageClient2;
    var ConsoleLogger = class {
      error(message) {
        (0, vscode_languageserver_protocol_1.RAL)().console.error(message);
      }
      warn(message) {
        (0, vscode_languageserver_protocol_1.RAL)().console.warn(message);
      }
      info(message) {
        (0, vscode_languageserver_protocol_1.RAL)().console.info(message);
      }
      log(message) {
        (0, vscode_languageserver_protocol_1.RAL)().console.log(message);
      }
    };
    function createConnection(input, output, errorHandler, closeHandler, options) {
      const logger = new ConsoleLogger();
      const connection = (0, vscode_languageserver_protocol_1.createProtocolConnection)(input, output, logger, options);
      connection.onError((data) => {
        errorHandler(data[0], data[1], data[2]);
      });
      connection.onClose(closeHandler);
      const result = {
        listen: () => connection.listen(),
        sendRequest: connection.sendRequest,
        onRequest: connection.onRequest,
        hasPendingResponse: connection.hasPendingResponse,
        sendNotification: connection.sendNotification,
        onNotification: connection.onNotification,
        onProgress: connection.onProgress,
        sendProgress: connection.sendProgress,
        trace: (value, tracer, sendNotificationOrTraceOptions) => {
          const defaultTraceOptions = {
            sendNotification: false,
            traceFormat: vscode_languageserver_protocol_1.TraceFormat.Text
          };
          if (sendNotificationOrTraceOptions === void 0) {
            return connection.trace(value, tracer, defaultTraceOptions);
          } else if (Is2.boolean(sendNotificationOrTraceOptions)) {
            return connection.trace(value, tracer, sendNotificationOrTraceOptions);
          } else {
            return connection.trace(value, tracer, sendNotificationOrTraceOptions);
          }
        },
        initialize: (params) => {
          return connection.sendRequest(vscode_languageserver_protocol_1.InitializeRequest.type, params);
        },
        shutdown: () => {
          return connection.sendRequest(vscode_languageserver_protocol_1.ShutdownRequest.type, void 0);
        },
        exit: () => {
          return connection.sendNotification(vscode_languageserver_protocol_1.ExitNotification.type);
        },
        end: () => connection.end(),
        dispose: () => connection.dispose()
      };
      return result;
    }
    var ProposedFeatures;
    (function(ProposedFeatures2) {
      function createAll(_client) {
        const result = [];
        return result;
      }
      ProposedFeatures2.createAll = createAll;
    })(ProposedFeatures || (exports2.ProposedFeatures = ProposedFeatures = {}));
  }
});

// node_modules/vscode-languageclient/lib/node/processes.js
var require_processes = __commonJS({
  "node_modules/vscode-languageclient/lib/node/processes.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.terminate = terminate;
    var cp = __importStar(require("child_process"));
    var isWindows = process.platform === "win32";
    var isMacintosh = process.platform === "darwin";
    var isLinux = process.platform === "linux";
    function terminate(process2, cwd) {
      if (isWindows) {
        try {
          const options = {
            stdio: ["pipe", "pipe", "ignore"]
          };
          if (cwd) {
            options.cwd = cwd;
          }
          cp.execFileSync("taskkill", ["/T", "/F", "/PID", process2.pid.toString()], options);
          return true;
        } catch (err) {
          return false;
        }
      } else if (isLinux || isMacintosh) {
        try {
          const pid = process2.pid.toString();
          if (!/^\d+$/.test(pid)) {
            return false;
          }
          const script = `
terminateTree() {
	for cpid in $(pgrep -P "$1"); do
		terminateTree "$cpid"
	done
	kill -9 "$1" > /dev/null 2>&1
}

terminateTree "${pid}"
`;
          const result = cp.spawnSync("/bin/sh", [], {
            input: script,
            stdio: ["pipe", "inherit", "inherit"]
          });
          return result.error ? false : true;
        } catch (err) {
          return false;
        }
      } else {
        process2.kill("SIGKILL");
        return true;
      }
    }
  }
});

// node_modules/vscode-jsonrpc/lib/node/ril.js
var require_ril = __commonJS({
  "node_modules/vscode-jsonrpc/lib/node/ril.js"(exports2) {
    "use strict";
    Object.defineProperty(exports2, "__esModule", { value: true });
    var util_1 = require("util");
    var api_1 = require_api();
    var MessageBuffer = class _MessageBuffer extends api_1.AbstractMessageBuffer {
      static emptyBuffer = Buffer.allocUnsafe(0);
      constructor(encoding = "utf-8") {
        super(encoding);
      }
      emptyBuffer() {
        return _MessageBuffer.emptyBuffer;
      }
      fromString(value, encoding) {
        return Buffer.from(value, encoding);
      }
      toString(value, encoding) {
        if (value instanceof Buffer) {
          return value.toString(encoding);
        } else {
          return new util_1.TextDecoder(encoding).decode(value);
        }
      }
      asNative(buffer, length) {
        if (length === void 0) {
          return buffer instanceof Buffer ? buffer : Buffer.from(buffer);
        } else {
          return buffer instanceof Buffer ? buffer.slice(0, length) : Buffer.from(buffer, 0, length);
        }
      }
      allocNative(length) {
        return Buffer.allocUnsafe(length);
      }
    };
    var ReadableStreamWrapper = class {
      stream;
      constructor(stream) {
        this.stream = stream;
      }
      onClose(listener) {
        this.stream.on("close", listener);
        return api_1.Disposable.create(() => this.stream.off("close", listener));
      }
      onError(listener) {
        this.stream.on("error", listener);
        return api_1.Disposable.create(() => this.stream.off("error", listener));
      }
      onEnd(listener) {
        this.stream.on("end", listener);
        return api_1.Disposable.create(() => this.stream.off("end", listener));
      }
      onData(listener) {
        this.stream.on("data", listener);
        return api_1.Disposable.create(() => this.stream.off("data", listener));
      }
    };
    var WritableStreamWrapper = class {
      stream;
      constructor(stream) {
        this.stream = stream;
      }
      onClose(listener) {
        this.stream.on("close", listener);
        return api_1.Disposable.create(() => this.stream.off("close", listener));
      }
      onError(listener) {
        this.stream.on("error", listener);
        return api_1.Disposable.create(() => this.stream.off("error", listener));
      }
      onEnd(listener) {
        this.stream.on("end", listener);
        return api_1.Disposable.create(() => this.stream.off("end", listener));
      }
      write(data, encoding) {
        return new Promise((resolve, reject) => {
          const callback = (error) => {
            if (error === void 0 || error === null) {
              resolve();
            } else {
              reject(error);
            }
          };
          if (typeof data === "string") {
            this.stream.write(data, encoding, callback);
          } else {
            this.stream.write(data, callback);
          }
        });
      }
      end() {
        this.stream.end();
      }
    };
    var _ril = Object.freeze({
      messageBuffer: Object.freeze({
        create: (encoding) => new MessageBuffer(encoding)
      }),
      applicationJson: Object.freeze({
        encoder: Object.freeze({
          name: "application/json",
          encode: (msg, options) => {
            try {
              return Promise.resolve(Buffer.from(JSON.stringify(msg, void 0, 0), options.charset));
            } catch (err) {
              return Promise.reject(err);
            }
          }
        }),
        decoder: Object.freeze({
          name: "application/json",
          decode: (buffer, options) => {
            try {
              if (buffer instanceof Buffer) {
                return Promise.resolve(JSON.parse(buffer.toString(options.charset)));
              } else {
                return Promise.resolve(JSON.parse(new util_1.TextDecoder(options.charset).decode(buffer)));
              }
            } catch (err) {
              return Promise.reject(err);
            }
          }
        })
      }),
      stream: Object.freeze({
        asReadableStream: (stream) => new ReadableStreamWrapper(stream),
        asWritableStream: (stream) => new WritableStreamWrapper(stream)
      }),
      console,
      timer: Object.freeze({
        setTimeout(callback, ms, ...args) {
          const handle = setTimeout(callback, ms, ...args);
          return { dispose: () => clearTimeout(handle) };
        },
        setImmediate(callback, ...args) {
          const handle = setImmediate(callback, ...args);
          return { dispose: () => clearImmediate(handle) };
        },
        setInterval(callback, ms, ...args) {
          const handle = setInterval(callback, ms, ...args);
          return { dispose: () => clearInterval(handle) };
        }
      })
    });
    function RIL() {
      return _ril;
    }
    (function(RIL2) {
      function install() {
        api_1.RAL.install(_ril);
      }
      RIL2.install = install;
    })(RIL || (RIL = {}));
    exports2.default = RIL;
  }
});

// node_modules/vscode-jsonrpc/lib/node/main.js
var require_main = __commonJS({
  "node_modules/vscode-jsonrpc/lib/node/main.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __exportStar = exports2 && exports2.__exportStar || function(m, exports3) {
      for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports3, p)) __createBinding(exports3, m, p);
    };
    var __importDefault = exports2 && exports2.__importDefault || function(mod) {
      return mod && mod.__esModule ? mod : { "default": mod };
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.StreamMessageWriter = exports2.StreamMessageReader = exports2.SocketMessageWriter = exports2.SocketMessageReader = exports2.PortMessageWriter = exports2.PortMessageReader = exports2.IPCMessageWriter = exports2.IPCMessageReader = void 0;
    exports2.generateRandomPipeName = generateRandomPipeName;
    exports2.createClientPipeTransport = createClientPipeTransport;
    exports2.createServerPipeTransport = createServerPipeTransport;
    exports2.createClientSocketTransport = createClientSocketTransport;
    exports2.createServerSocketTransport = createServerSocketTransport;
    exports2.createMessageConnection = createMessageConnection;
    var ril_1 = __importDefault(require_ril());
    ril_1.default.install();
    var path = __importStar(require("path"));
    var os = __importStar(require("os"));
    var fs = __importStar(require("fs"));
    var crypto_1 = require("crypto");
    var net_1 = require("net");
    var api_1 = require_api();
    __exportStar(require_api(), exports2);
    var IPCMessageReader = class extends api_1.AbstractMessageReader {
      process;
      constructor(process2) {
        super();
        this.process = process2;
        const eventEmitter = this.process;
        eventEmitter.on("error", (error) => this.fireError(error));
        eventEmitter.on("close", () => this.fireClose());
      }
      listen(callback) {
        this.process.on("message", callback);
        return api_1.Disposable.create(() => this.process.off("message", callback));
      }
    };
    exports2.IPCMessageReader = IPCMessageReader;
    var IPCMessageWriter = class extends api_1.AbstractMessageWriter {
      process;
      errorCount;
      constructor(process2) {
        super();
        this.process = process2;
        this.errorCount = 0;
        const eventEmitter = this.process;
        eventEmitter.on("error", (error) => this.fireError(error));
        eventEmitter.on("close", () => this.fireClose);
      }
      write(msg) {
        try {
          if (typeof this.process.send === "function") {
            this.process.send(msg, void 0, void 0, (error) => {
              if (error) {
                this.errorCount++;
                this.handleError(error, msg);
              } else {
                this.errorCount = 0;
              }
            });
          }
          return Promise.resolve();
        } catch (error) {
          this.handleError(error, msg);
          return Promise.reject(error);
        }
      }
      handleError(error, msg) {
        this.errorCount++;
        this.fireError(error, msg, this.errorCount);
      }
      end() {
      }
    };
    exports2.IPCMessageWriter = IPCMessageWriter;
    var PortMessageReader = class extends api_1.AbstractMessageReader {
      onData;
      constructor(port) {
        super();
        this.onData = new api_1.Emitter();
        port.on("close", () => this.fireClose);
        port.on("error", (error) => this.fireError(error));
        port.on("message", (message) => {
          this.onData.fire(message);
        });
      }
      listen(callback) {
        return this.onData.event(callback);
      }
    };
    exports2.PortMessageReader = PortMessageReader;
    var PortMessageWriter = class extends api_1.AbstractMessageWriter {
      port;
      errorCount;
      constructor(port) {
        super();
        this.port = port;
        this.errorCount = 0;
        port.on("close", () => this.fireClose());
        port.on("error", (error) => this.fireError(error));
      }
      write(msg) {
        try {
          this.port.postMessage(msg);
          return Promise.resolve();
        } catch (error) {
          this.handleError(error, msg);
          return Promise.reject(error);
        }
      }
      handleError(error, msg) {
        this.errorCount++;
        this.fireError(error, msg, this.errorCount);
      }
      end() {
      }
    };
    exports2.PortMessageWriter = PortMessageWriter;
    var SocketMessageReader = class extends api_1.ReadableStreamMessageReader {
      constructor(socket, encoding = "utf-8") {
        super((0, ril_1.default)().stream.asReadableStream(socket), encoding);
      }
    };
    exports2.SocketMessageReader = SocketMessageReader;
    var SocketMessageWriter = class extends api_1.WriteableStreamMessageWriter {
      socket;
      constructor(socket, options) {
        super((0, ril_1.default)().stream.asWritableStream(socket), options);
        this.socket = socket;
      }
      dispose() {
        super.dispose();
        this.socket.destroy();
      }
    };
    exports2.SocketMessageWriter = SocketMessageWriter;
    var StreamMessageReader = class extends api_1.ReadableStreamMessageReader {
      constructor(readable, encoding) {
        super((0, ril_1.default)().stream.asReadableStream(readable), encoding);
      }
    };
    exports2.StreamMessageReader = StreamMessageReader;
    var StreamMessageWriter = class extends api_1.WriteableStreamMessageWriter {
      constructor(writable, options) {
        super((0, ril_1.default)().stream.asWritableStream(writable), options);
      }
    };
    exports2.StreamMessageWriter = StreamMessageWriter;
    var XDG_RUNTIME_DIR = process.env["XDG_RUNTIME_DIR"];
    var safeIpcPathLengths = /* @__PURE__ */ new Map([
      ["linux", 107],
      ["darwin", 102]
    ]);
    function generateRandomPipeName() {
      if (process.platform === "win32") {
        return `\\\\.\\pipe\\lsp-${(0, crypto_1.randomBytes)(16).toString("hex")}-sock`;
      }
      let randomLength = 32;
      const fixedLength = "/lsp-.sock".length;
      const tmpDir = fs.realpathSync(XDG_RUNTIME_DIR ?? os.tmpdir());
      const limit = safeIpcPathLengths.get(process.platform);
      if (limit !== void 0) {
        randomLength = Math.min(limit - tmpDir.length - fixedLength, randomLength);
      }
      if (randomLength < 16) {
        throw new Error(`Unable to generate a random pipe name with ${randomLength} characters.`);
      }
      const randomSuffix = (0, crypto_1.randomBytes)(Math.floor(randomLength / 2)).toString("hex");
      return path.join(tmpDir, `lsp-${randomSuffix}.sock`);
    }
    function createClientPipeTransport(pipeName, encoding = "utf-8") {
      let connectResolve;
      const connected = new Promise((resolve, _reject) => {
        connectResolve = resolve;
      });
      return new Promise((resolve, reject) => {
        const server = (0, net_1.createServer)((socket) => {
          server.close();
          connectResolve([
            new SocketMessageReader(socket, encoding),
            new SocketMessageWriter(socket, encoding)
          ]);
        });
        server.on("error", reject);
        server.listen(pipeName, () => {
          server.removeListener("error", reject);
          resolve({
            onConnected: () => {
              return connected;
            }
          });
        });
      });
    }
    function createServerPipeTransport(pipeName, encoding = "utf-8") {
      const socket = (0, net_1.createConnection)(pipeName);
      return [
        new SocketMessageReader(socket, encoding),
        new SocketMessageWriter(socket, encoding)
      ];
    }
    function createClientSocketTransport(port, encoding = "utf-8") {
      let connectResolve;
      const connected = new Promise((resolve, _reject) => {
        connectResolve = resolve;
      });
      return new Promise((resolve, reject) => {
        const server = (0, net_1.createServer)((socket) => {
          server.close();
          connectResolve([
            new SocketMessageReader(socket, encoding),
            new SocketMessageWriter(socket, encoding)
          ]);
        });
        server.on("error", reject);
        server.listen(port, "127.0.0.1", () => {
          server.removeListener("error", reject);
          resolve({
            onConnected: () => {
              return connected;
            }
          });
        });
      });
    }
    function createServerSocketTransport(port, encoding = "utf-8") {
      const socket = (0, net_1.createConnection)(port, "127.0.0.1");
      return [
        new SocketMessageReader(socket, encoding),
        new SocketMessageWriter(socket, encoding)
      ];
    }
    function isReadableStream(value) {
      const candidate = value;
      return candidate.read !== void 0 && candidate.addListener !== void 0;
    }
    function isWritableStream(value) {
      const candidate = value;
      return candidate.write !== void 0 && candidate.addListener !== void 0;
    }
    function createMessageConnection(input, output, logger, options) {
      if (!logger) {
        logger = api_1.NullLogger;
      }
      const reader = isReadableStream(input) ? new StreamMessageReader(input) : input;
      const writer = isWritableStream(output) ? new StreamMessageWriter(output) : output;
      if (api_1.ConnectionStrategy.is(options)) {
        options = { connectionStrategy: options };
      }
      return (0, api_1.createMessageConnection)(reader, writer, logger, options);
    }
  }
});

// node_modules/vscode-languageserver-protocol/lib/node/main.js
var require_main2 = __commonJS({
  "node_modules/vscode-languageserver-protocol/lib/node/main.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __exportStar = exports2 && exports2.__exportStar || function(m, exports3) {
      for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports3, p)) __createBinding(exports3, m, p);
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.createProtocolConnection = createProtocolConnection;
    var node_1 = require_main();
    __exportStar(require_main(), exports2);
    __exportStar(require_api2(), exports2);
    function createProtocolConnection(input, output, logger, options) {
      return (0, node_1.createMessageConnection)(input, output, logger, options);
    }
  }
});

// node_modules/semver/internal/debug.js
var require_debug = __commonJS({
  "node_modules/semver/internal/debug.js"(exports2, module2) {
    "use strict";
    var debug = typeof process === "object" && process.env && process.env.NODE_DEBUG && /\bsemver\b/i.test(process.env.NODE_DEBUG) ? (...args) => console.error("SEMVER", ...args) : () => {
    };
    module2.exports = debug;
  }
});

// node_modules/semver/internal/constants.js
var require_constants = __commonJS({
  "node_modules/semver/internal/constants.js"(exports2, module2) {
    "use strict";
    var SEMVER_SPEC_VERSION = "2.0.0";
    var MAX_LENGTH = 256;
    var MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER || /* istanbul ignore next */
    9007199254740991;
    var MAX_SAFE_COMPONENT_LENGTH = 16;
    var MAX_SAFE_BUILD_LENGTH = MAX_LENGTH - 6;
    var RELEASE_TYPES = [
      "major",
      "premajor",
      "minor",
      "preminor",
      "patch",
      "prepatch",
      "prerelease"
    ];
    module2.exports = {
      MAX_LENGTH,
      MAX_SAFE_COMPONENT_LENGTH,
      MAX_SAFE_BUILD_LENGTH,
      MAX_SAFE_INTEGER,
      RELEASE_TYPES,
      SEMVER_SPEC_VERSION,
      FLAG_INCLUDE_PRERELEASE: 1,
      FLAG_LOOSE: 2
    };
  }
});

// node_modules/semver/internal/re.js
var require_re = __commonJS({
  "node_modules/semver/internal/re.js"(exports2, module2) {
    "use strict";
    var {
      MAX_SAFE_COMPONENT_LENGTH,
      MAX_SAFE_BUILD_LENGTH,
      MAX_LENGTH
    } = require_constants();
    var debug = require_debug();
    exports2 = module2.exports = {};
    var re = exports2.re = [];
    var safeRe = exports2.safeRe = [];
    var src = exports2.src = [];
    var safeSrc = exports2.safeSrc = [];
    var t = exports2.t = {};
    var R = 0;
    var LETTERDASHNUMBER = "[a-zA-Z0-9-]";
    var safeRegexReplacements = [
      ["\\s", 1],
      ["\\d", MAX_LENGTH],
      [LETTERDASHNUMBER, MAX_SAFE_BUILD_LENGTH]
    ];
    var makeSafeRegex = (value) => {
      for (const [token, max] of safeRegexReplacements) {
        value = value.split(`${token}*`).join(`${token}{0,${max}}`).split(`${token}+`).join(`${token}{1,${max}}`);
      }
      return value;
    };
    var createToken = (name, value, isGlobal) => {
      const safe = makeSafeRegex(value);
      const index = R++;
      debug(name, index, value);
      t[name] = index;
      src[index] = value;
      safeSrc[index] = safe;
      re[index] = new RegExp(value, isGlobal ? "g" : void 0);
      safeRe[index] = new RegExp(safe, isGlobal ? "g" : void 0);
    };
    createToken("NUMERICIDENTIFIER", "0|[1-9]\\d*");
    createToken("NUMERICIDENTIFIERLOOSE", "\\d+");
    createToken("NONNUMERICIDENTIFIER", `\\d*[a-zA-Z-]${LETTERDASHNUMBER}*`);
    createToken("MAINVERSION", `(${src[t.NUMERICIDENTIFIER]})\\.(${src[t.NUMERICIDENTIFIER]})\\.(${src[t.NUMERICIDENTIFIER]})`);
    createToken("MAINVERSIONLOOSE", `(${src[t.NUMERICIDENTIFIERLOOSE]})\\.(${src[t.NUMERICIDENTIFIERLOOSE]})\\.(${src[t.NUMERICIDENTIFIERLOOSE]})`);
    createToken("PRERELEASEIDENTIFIER", `(?:${src[t.NONNUMERICIDENTIFIER]}|${src[t.NUMERICIDENTIFIER]})`);
    createToken("PRERELEASEIDENTIFIERLOOSE", `(?:${src[t.NONNUMERICIDENTIFIER]}|${src[t.NUMERICIDENTIFIERLOOSE]})`);
    createToken("PRERELEASE", `(?:-(${src[t.PRERELEASEIDENTIFIER]}(?:\\.${src[t.PRERELEASEIDENTIFIER]})*))`);
    createToken("PRERELEASELOOSE", `(?:-?(${src[t.PRERELEASEIDENTIFIERLOOSE]}(?:\\.${src[t.PRERELEASEIDENTIFIERLOOSE]})*))`);
    createToken("BUILDIDENTIFIER", `${LETTERDASHNUMBER}+`);
    createToken("BUILD", `(?:\\+(${src[t.BUILDIDENTIFIER]}(?:\\.${src[t.BUILDIDENTIFIER]})*))`);
    createToken("FULLPLAIN", `v?${src[t.MAINVERSION]}${src[t.PRERELEASE]}?${src[t.BUILD]}?`);
    createToken("FULL", `^${src[t.FULLPLAIN]}$`);
    createToken("LOOSEPLAIN", `[v=\\s]*${src[t.MAINVERSIONLOOSE]}${src[t.PRERELEASELOOSE]}?${src[t.BUILD]}?`);
    createToken("LOOSE", `^${src[t.LOOSEPLAIN]}$`);
    createToken("GTLT", "((?:<|>)?=?)");
    createToken("XRANGEIDENTIFIERLOOSE", `${src[t.NUMERICIDENTIFIERLOOSE]}|x|X|\\*`);
    createToken("XRANGEIDENTIFIER", `${src[t.NUMERICIDENTIFIER]}|x|X|\\*`);
    createToken("XRANGEPLAIN", `[v=\\s]*(${src[t.XRANGEIDENTIFIER]})(?:\\.(${src[t.XRANGEIDENTIFIER]})(?:\\.(${src[t.XRANGEIDENTIFIER]})(?:${src[t.PRERELEASE]})?${src[t.BUILD]}?)?)?`);
    createToken("XRANGEPLAINLOOSE", `[v=\\s]*(${src[t.XRANGEIDENTIFIERLOOSE]})(?:\\.(${src[t.XRANGEIDENTIFIERLOOSE]})(?:\\.(${src[t.XRANGEIDENTIFIERLOOSE]})(?:${src[t.PRERELEASELOOSE]})?${src[t.BUILD]}?)?)?`);
    createToken("XRANGE", `^${src[t.GTLT]}\\s*${src[t.XRANGEPLAIN]}$`);
    createToken("XRANGELOOSE", `^${src[t.GTLT]}\\s*${src[t.XRANGEPLAINLOOSE]}$`);
    createToken("COERCEPLAIN", `${"(^|[^\\d])(\\d{1,"}${MAX_SAFE_COMPONENT_LENGTH}})(?:\\.(\\d{1,${MAX_SAFE_COMPONENT_LENGTH}}))?(?:\\.(\\d{1,${MAX_SAFE_COMPONENT_LENGTH}}))?`);
    createToken("COERCE", `${src[t.COERCEPLAIN]}(?:$|[^\\d])`);
    createToken("COERCEFULL", src[t.COERCEPLAIN] + `(?:${src[t.PRERELEASE]})?(?:${src[t.BUILD]})?(?:$|[^\\d])`);
    createToken("COERCERTL", src[t.COERCE], true);
    createToken("COERCERTLFULL", src[t.COERCEFULL], true);
    createToken("LONETILDE", "(?:~>?)");
    createToken("TILDETRIM", `(\\s*)${src[t.LONETILDE]}\\s+`, true);
    exports2.tildeTrimReplace = "$1~";
    createToken("TILDE", `^${src[t.LONETILDE]}${src[t.XRANGEPLAIN]}$`);
    createToken("TILDELOOSE", `^${src[t.LONETILDE]}${src[t.XRANGEPLAINLOOSE]}$`);
    createToken("LONECARET", "(?:\\^)");
    createToken("CARETTRIM", `(\\s*)${src[t.LONECARET]}\\s+`, true);
    exports2.caretTrimReplace = "$1^";
    createToken("CARET", `^${src[t.LONECARET]}${src[t.XRANGEPLAIN]}$`);
    createToken("CARETLOOSE", `^${src[t.LONECARET]}${src[t.XRANGEPLAINLOOSE]}$`);
    createToken("COMPARATORLOOSE", `^${src[t.GTLT]}\\s*(${src[t.LOOSEPLAIN]})$|^$`);
    createToken("COMPARATOR", `^${src[t.GTLT]}\\s*(${src[t.FULLPLAIN]})$|^$`);
    createToken("COMPARATORTRIM", `(\\s*)${src[t.GTLT]}\\s*(${src[t.LOOSEPLAIN]}|${src[t.XRANGEPLAIN]})`, true);
    exports2.comparatorTrimReplace = "$1$2$3";
    createToken("HYPHENRANGE", `^\\s*(${src[t.XRANGEPLAIN]})\\s+-\\s+(${src[t.XRANGEPLAIN]})\\s*$`);
    createToken("HYPHENRANGELOOSE", `^\\s*(${src[t.XRANGEPLAINLOOSE]})\\s+-\\s+(${src[t.XRANGEPLAINLOOSE]})\\s*$`);
    createToken("STAR", "(<|>)?=?\\s*\\*");
    createToken("GTE0", "^\\s*>=\\s*0\\.0\\.0\\s*$");
    createToken("GTE0PRE", "^\\s*>=\\s*0\\.0\\.0-0\\s*$");
  }
});

// node_modules/semver/internal/parse-options.js
var require_parse_options = __commonJS({
  "node_modules/semver/internal/parse-options.js"(exports2, module2) {
    "use strict";
    var looseOption = Object.freeze({ loose: true });
    var emptyOpts = Object.freeze({});
    var parseOptions = (options) => {
      if (!options) {
        return emptyOpts;
      }
      if (typeof options !== "object") {
        return looseOption;
      }
      return options;
    };
    module2.exports = parseOptions;
  }
});

// node_modules/semver/internal/identifiers.js
var require_identifiers = __commonJS({
  "node_modules/semver/internal/identifiers.js"(exports2, module2) {
    "use strict";
    var numeric = /^[0-9]+$/;
    var compareIdentifiers = (a, b) => {
      if (typeof a === "number" && typeof b === "number") {
        return a === b ? 0 : a < b ? -1 : 1;
      }
      const anum = numeric.test(a);
      const bnum = numeric.test(b);
      if (anum && bnum) {
        a = +a;
        b = +b;
      }
      return a === b ? 0 : anum && !bnum ? -1 : bnum && !anum ? 1 : a < b ? -1 : 1;
    };
    var rcompareIdentifiers = (a, b) => compareIdentifiers(b, a);
    module2.exports = {
      compareIdentifiers,
      rcompareIdentifiers
    };
  }
});

// node_modules/semver/classes/semver.js
var require_semver = __commonJS({
  "node_modules/semver/classes/semver.js"(exports2, module2) {
    "use strict";
    var debug = require_debug();
    var { MAX_LENGTH, MAX_SAFE_INTEGER } = require_constants();
    var { safeRe: re, t } = require_re();
    var parseOptions = require_parse_options();
    var { compareIdentifiers } = require_identifiers();
    var isPrereleaseIdentifier = (prerelease, identifier) => {
      const identifiers = identifier.split(".");
      if (identifiers.length > prerelease.length) {
        return false;
      }
      for (let i = 0; i < identifiers.length; i++) {
        if (compareIdentifiers(prerelease[i], identifiers[i]) !== 0) {
          return false;
        }
      }
      return true;
    };
    var SemVer = class _SemVer {
      constructor(version, options) {
        options = parseOptions(options);
        if (version instanceof _SemVer) {
          if (version.loose === !!options.loose && version.includePrerelease === !!options.includePrerelease) {
            return version;
          } else {
            version = version.version;
          }
        } else if (typeof version !== "string") {
          throw new TypeError(`Invalid version. Must be a string. Got type "${typeof version}".`);
        }
        if (version.length > MAX_LENGTH) {
          throw new TypeError(
            `version is longer than ${MAX_LENGTH} characters`
          );
        }
        debug("SemVer", version, options);
        this.options = options;
        this.loose = !!options.loose;
        this.includePrerelease = !!options.includePrerelease;
        const m = version.trim().match(options.loose ? re[t.LOOSE] : re[t.FULL]);
        if (!m) {
          throw new TypeError(`Invalid Version: ${version}`);
        }
        this.raw = version;
        this.major = +m[1];
        this.minor = +m[2];
        this.patch = +m[3];
        if (this.major > MAX_SAFE_INTEGER || this.major < 0) {
          throw new TypeError("Invalid major version");
        }
        if (this.minor > MAX_SAFE_INTEGER || this.minor < 0) {
          throw new TypeError("Invalid minor version");
        }
        if (this.patch > MAX_SAFE_INTEGER || this.patch < 0) {
          throw new TypeError("Invalid patch version");
        }
        if (!m[4]) {
          this.prerelease = [];
        } else {
          this.prerelease = m[4].split(".").map((id) => {
            if (/^[0-9]+$/.test(id)) {
              const num = +id;
              if (num >= 0 && num < MAX_SAFE_INTEGER) {
                return num;
              }
            }
            return id;
          });
        }
        this.build = m[5] ? m[5].split(".") : [];
        this.format();
      }
      format() {
        this.version = `${this.major}.${this.minor}.${this.patch}`;
        if (this.prerelease.length) {
          this.version += `-${this.prerelease.join(".")}`;
        }
        return this.version;
      }
      toString() {
        return this.version;
      }
      compare(other) {
        debug("SemVer.compare", this.version, this.options, other);
        if (!(other instanceof _SemVer)) {
          if (typeof other === "string" && other === this.version) {
            return 0;
          }
          other = new _SemVer(other, this.options);
        }
        if (other.version === this.version) {
          return 0;
        }
        return this.compareMain(other) || this.comparePre(other);
      }
      compareMain(other) {
        if (!(other instanceof _SemVer)) {
          other = new _SemVer(other, this.options);
        }
        if (this.major < other.major) {
          return -1;
        }
        if (this.major > other.major) {
          return 1;
        }
        if (this.minor < other.minor) {
          return -1;
        }
        if (this.minor > other.minor) {
          return 1;
        }
        if (this.patch < other.patch) {
          return -1;
        }
        if (this.patch > other.patch) {
          return 1;
        }
        return 0;
      }
      comparePre(other) {
        if (!(other instanceof _SemVer)) {
          other = new _SemVer(other, this.options);
        }
        if (this.prerelease.length && !other.prerelease.length) {
          return -1;
        } else if (!this.prerelease.length && other.prerelease.length) {
          return 1;
        } else if (!this.prerelease.length && !other.prerelease.length) {
          return 0;
        }
        let i = 0;
        do {
          const a = this.prerelease[i];
          const b = other.prerelease[i];
          debug("prerelease compare", i, a, b);
          if (a === void 0 && b === void 0) {
            return 0;
          } else if (b === void 0) {
            return 1;
          } else if (a === void 0) {
            return -1;
          } else if (a === b) {
            continue;
          } else {
            return compareIdentifiers(a, b);
          }
        } while (++i);
      }
      compareBuild(other) {
        if (!(other instanceof _SemVer)) {
          other = new _SemVer(other, this.options);
        }
        let i = 0;
        do {
          const a = this.build[i];
          const b = other.build[i];
          debug("build compare", i, a, b);
          if (a === void 0 && b === void 0) {
            return 0;
          } else if (b === void 0) {
            return 1;
          } else if (a === void 0) {
            return -1;
          } else if (a === b) {
            continue;
          } else {
            return compareIdentifiers(a, b);
          }
        } while (++i);
      }
      // preminor will bump the version up to the next minor release, and immediately
      // down to pre-release. premajor and prepatch work the same way.
      inc(release, identifier, identifierBase) {
        if (release.startsWith("pre")) {
          if (!identifier && identifierBase === false) {
            throw new Error("invalid increment argument: identifier is empty");
          }
          if (identifier) {
            const match = `-${identifier}`.match(this.options.loose ? re[t.PRERELEASELOOSE] : re[t.PRERELEASE]);
            if (!match || match[1] !== identifier) {
              throw new Error(`invalid identifier: ${identifier}`);
            }
          }
        }
        switch (release) {
          case "premajor":
            this.prerelease.length = 0;
            this.patch = 0;
            this.minor = 0;
            this.major++;
            this.inc("pre", identifier, identifierBase);
            break;
          case "preminor":
            this.prerelease.length = 0;
            this.patch = 0;
            this.minor++;
            this.inc("pre", identifier, identifierBase);
            break;
          case "prepatch":
            this.prerelease.length = 0;
            this.inc("patch", identifier, identifierBase);
            this.inc("pre", identifier, identifierBase);
            break;
          // If the input is a non-prerelease version, this acts the same as
          // prepatch.
          case "prerelease":
            if (this.prerelease.length === 0) {
              this.inc("patch", identifier, identifierBase);
            }
            this.inc("pre", identifier, identifierBase);
            break;
          case "release":
            if (this.prerelease.length === 0) {
              throw new Error(`version ${this.raw} is not a prerelease`);
            }
            this.prerelease.length = 0;
            break;
          case "major":
            if (this.minor !== 0 || this.patch !== 0 || this.prerelease.length === 0) {
              this.major++;
            }
            this.minor = 0;
            this.patch = 0;
            this.prerelease = [];
            break;
          case "minor":
            if (this.patch !== 0 || this.prerelease.length === 0) {
              this.minor++;
            }
            this.patch = 0;
            this.prerelease = [];
            break;
          case "patch":
            if (this.prerelease.length === 0) {
              this.patch++;
            }
            this.prerelease = [];
            break;
          // This probably shouldn't be used publicly.
          // 1.0.0 'pre' would become 1.0.0-0 which is the wrong direction.
          case "pre": {
            const base = Number(identifierBase) ? 1 : 0;
            if (this.prerelease.length === 0) {
              this.prerelease = [base];
            } else {
              let i = this.prerelease.length;
              while (--i >= 0) {
                if (typeof this.prerelease[i] === "number") {
                  this.prerelease[i]++;
                  i = -2;
                }
              }
              if (i === -1) {
                if (identifier === this.prerelease.join(".") && identifierBase === false) {
                  throw new Error("invalid increment argument: identifier already exists");
                }
                this.prerelease.push(base);
              }
            }
            if (identifier) {
              let prerelease = [identifier, base];
              if (identifierBase === false) {
                prerelease = [identifier];
              }
              if (isPrereleaseIdentifier(this.prerelease, identifier)) {
                const prereleaseBase = this.prerelease[identifier.split(".").length];
                if (isNaN(prereleaseBase)) {
                  this.prerelease = prerelease;
                }
              } else {
                this.prerelease = prerelease;
              }
            }
            break;
          }
          default:
            throw new Error(`invalid increment argument: ${release}`);
        }
        this.raw = this.format();
        if (this.build.length) {
          this.raw += `+${this.build.join(".")}`;
        }
        return this;
      }
    };
    module2.exports = SemVer;
  }
});

// node_modules/semver/functions/parse.js
var require_parse = __commonJS({
  "node_modules/semver/functions/parse.js"(exports2, module2) {
    "use strict";
    var SemVer = require_semver();
    var parse = (version, options, throwErrors = false) => {
      if (version instanceof SemVer) {
        return version;
      }
      try {
        return new SemVer(version, options);
      } catch (er) {
        if (!throwErrors) {
          return null;
        }
        throw er;
      }
    };
    module2.exports = parse;
  }
});

// node_modules/semver/internal/lrucache.js
var require_lrucache = __commonJS({
  "node_modules/semver/internal/lrucache.js"(exports2, module2) {
    "use strict";
    var LRUCache = class {
      constructor() {
        this.max = 1e3;
        this.map = /* @__PURE__ */ new Map();
      }
      get(key) {
        const value = this.map.get(key);
        if (value === void 0) {
          return void 0;
        } else {
          this.map.delete(key);
          this.map.set(key, value);
          return value;
        }
      }
      delete(key) {
        return this.map.delete(key);
      }
      set(key, value) {
        const deleted = this.delete(key);
        if (!deleted && value !== void 0) {
          if (this.map.size >= this.max) {
            const firstKey = this.map.keys().next().value;
            this.delete(firstKey);
          }
          this.map.set(key, value);
        }
        return this;
      }
    };
    module2.exports = LRUCache;
  }
});

// node_modules/semver/functions/compare.js
var require_compare = __commonJS({
  "node_modules/semver/functions/compare.js"(exports2, module2) {
    "use strict";
    var SemVer = require_semver();
    var compare = (a, b, loose) => new SemVer(a, loose).compare(new SemVer(b, loose));
    module2.exports = compare;
  }
});

// node_modules/semver/functions/eq.js
var require_eq = __commonJS({
  "node_modules/semver/functions/eq.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var eq = (a, b, loose) => compare(a, b, loose) === 0;
    module2.exports = eq;
  }
});

// node_modules/semver/functions/neq.js
var require_neq = __commonJS({
  "node_modules/semver/functions/neq.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var neq = (a, b, loose) => compare(a, b, loose) !== 0;
    module2.exports = neq;
  }
});

// node_modules/semver/functions/gt.js
var require_gt = __commonJS({
  "node_modules/semver/functions/gt.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var gt = (a, b, loose) => compare(a, b, loose) > 0;
    module2.exports = gt;
  }
});

// node_modules/semver/functions/gte.js
var require_gte = __commonJS({
  "node_modules/semver/functions/gte.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var gte = (a, b, loose) => compare(a, b, loose) >= 0;
    module2.exports = gte;
  }
});

// node_modules/semver/functions/lt.js
var require_lt = __commonJS({
  "node_modules/semver/functions/lt.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var lt = (a, b, loose) => compare(a, b, loose) < 0;
    module2.exports = lt;
  }
});

// node_modules/semver/functions/lte.js
var require_lte = __commonJS({
  "node_modules/semver/functions/lte.js"(exports2, module2) {
    "use strict";
    var compare = require_compare();
    var lte = (a, b, loose) => compare(a, b, loose) <= 0;
    module2.exports = lte;
  }
});

// node_modules/semver/functions/cmp.js
var require_cmp = __commonJS({
  "node_modules/semver/functions/cmp.js"(exports2, module2) {
    "use strict";
    var eq = require_eq();
    var neq = require_neq();
    var gt = require_gt();
    var gte = require_gte();
    var lt = require_lt();
    var lte = require_lte();
    var cmp = (a, op, b, loose) => {
      switch (op) {
        case "===":
          if (typeof a === "object") {
            a = a.version;
          }
          if (typeof b === "object") {
            b = b.version;
          }
          return a === b;
        case "!==":
          if (typeof a === "object") {
            a = a.version;
          }
          if (typeof b === "object") {
            b = b.version;
          }
          return a !== b;
        case "":
        case "=":
        case "==":
          return eq(a, b, loose);
        case "!=":
          return neq(a, b, loose);
        case ">":
          return gt(a, b, loose);
        case ">=":
          return gte(a, b, loose);
        case "<":
          return lt(a, b, loose);
        case "<=":
          return lte(a, b, loose);
        default:
          throw new TypeError(`Invalid operator: ${op}`);
      }
    };
    module2.exports = cmp;
  }
});

// node_modules/semver/classes/comparator.js
var require_comparator = __commonJS({
  "node_modules/semver/classes/comparator.js"(exports2, module2) {
    "use strict";
    var ANY = /* @__PURE__ */ Symbol("SemVer ANY");
    var Comparator = class _Comparator {
      static get ANY() {
        return ANY;
      }
      constructor(comp, options) {
        options = parseOptions(options);
        if (comp instanceof _Comparator) {
          if (comp.loose === !!options.loose) {
            return comp;
          } else {
            comp = comp.value;
          }
        }
        comp = comp.trim().split(/\s+/).join(" ");
        debug("comparator", comp, options);
        this.options = options;
        this.loose = !!options.loose;
        this.parse(comp);
        if (this.semver === ANY) {
          this.value = "";
        } else {
          this.value = this.operator + this.semver.version;
        }
        debug("comp", this);
      }
      parse(comp) {
        const r = this.options.loose ? re[t.COMPARATORLOOSE] : re[t.COMPARATOR];
        const m = comp.match(r);
        if (!m) {
          throw new TypeError(`Invalid comparator: ${comp}`);
        }
        this.operator = m[1] !== void 0 ? m[1] : "";
        if (this.operator === "=") {
          this.operator = "";
        }
        if (!m[2]) {
          this.semver = ANY;
        } else {
          this.semver = new SemVer(m[2], this.options.loose);
        }
      }
      toString() {
        return this.value;
      }
      test(version) {
        debug("Comparator.test", version, this.options.loose);
        if (this.semver === ANY || version === ANY) {
          return true;
        }
        if (typeof version === "string") {
          try {
            version = new SemVer(version, this.options);
          } catch (er) {
            return false;
          }
        }
        return cmp(version, this.operator, this.semver, this.options);
      }
      intersects(comp, options) {
        if (!(comp instanceof _Comparator)) {
          throw new TypeError("a Comparator is required");
        }
        if (this.operator === "") {
          if (this.value === "") {
            return true;
          }
          return new Range2(comp.value, options).test(this.value);
        } else if (comp.operator === "") {
          if (comp.value === "") {
            return true;
          }
          return new Range2(this.value, options).test(comp.semver);
        }
        options = parseOptions(options);
        if (options.includePrerelease && (this.value === "<0.0.0-0" || comp.value === "<0.0.0-0")) {
          return false;
        }
        if (!options.includePrerelease && (this.value.startsWith("<0.0.0") || comp.value.startsWith("<0.0.0"))) {
          return false;
        }
        if (this.operator.startsWith(">") && comp.operator.startsWith(">")) {
          return true;
        }
        if (this.operator.startsWith("<") && comp.operator.startsWith("<")) {
          return true;
        }
        if (this.semver.version === comp.semver.version && this.operator.includes("=") && comp.operator.includes("=")) {
          return true;
        }
        if (cmp(this.semver, "<", comp.semver, options) && this.operator.startsWith(">") && comp.operator.startsWith("<")) {
          return true;
        }
        if (cmp(this.semver, ">", comp.semver, options) && this.operator.startsWith("<") && comp.operator.startsWith(">")) {
          return true;
        }
        return false;
      }
    };
    module2.exports = Comparator;
    var parseOptions = require_parse_options();
    var { safeRe: re, t } = require_re();
    var cmp = require_cmp();
    var debug = require_debug();
    var SemVer = require_semver();
    var Range2 = require_range();
  }
});

// node_modules/semver/classes/range.js
var require_range = __commonJS({
  "node_modules/semver/classes/range.js"(exports2, module2) {
    "use strict";
    var SPACE_CHARACTERS = /\s+/g;
    var Range2 = class _Range {
      constructor(range, options) {
        options = parseOptions(options);
        if (range instanceof _Range) {
          if (range.loose === !!options.loose && range.includePrerelease === !!options.includePrerelease) {
            return range;
          } else {
            return new _Range(range.raw, options);
          }
        }
        if (range instanceof Comparator) {
          this.raw = range.value;
          this.set = [[range]];
          this.formatted = void 0;
          return this;
        }
        this.options = options;
        this.loose = !!options.loose;
        this.includePrerelease = !!options.includePrerelease;
        this.raw = range.trim().replace(SPACE_CHARACTERS, " ");
        this.set = this.raw.split("||").map((r) => this.parseRange(r.trim())).filter((c) => c.length);
        if (!this.set.length) {
          throw new TypeError(`Invalid SemVer Range: ${this.raw}`);
        }
        if (this.set.length > 1) {
          const first = this.set[0];
          this.set = this.set.filter((c) => !isNullSet(c[0]));
          if (this.set.length === 0) {
            this.set = [first];
          } else if (this.set.length > 1) {
            for (const c of this.set) {
              if (c.length === 1 && isAny(c[0])) {
                this.set = [c];
                break;
              }
            }
          }
        }
        this.formatted = void 0;
      }
      get range() {
        if (this.formatted === void 0) {
          this.formatted = "";
          for (let i = 0; i < this.set.length; i++) {
            if (i > 0) {
              this.formatted += "||";
            }
            const comps = this.set[i];
            for (let k = 0; k < comps.length; k++) {
              if (k > 0) {
                this.formatted += " ";
              }
              this.formatted += comps[k].toString().trim();
            }
          }
        }
        return this.formatted;
      }
      format() {
        return this.range;
      }
      toString() {
        return this.range;
      }
      parseRange(range) {
        range = range.replace(BUILDSTRIPRE, "");
        const memoOpts = (this.options.includePrerelease && FLAG_INCLUDE_PRERELEASE) | (this.options.loose && FLAG_LOOSE);
        const memoKey = memoOpts + ":" + range;
        const cached = cache.get(memoKey);
        if (cached) {
          return cached;
        }
        const loose = this.options.loose;
        const hr = loose ? re[t.HYPHENRANGELOOSE] : re[t.HYPHENRANGE];
        range = range.replace(hr, hyphenReplace(this.options.includePrerelease));
        debug("hyphen replace", range);
        range = range.replace(re[t.COMPARATORTRIM], comparatorTrimReplace);
        debug("comparator trim", range);
        range = range.replace(re[t.TILDETRIM], tildeTrimReplace);
        debug("tilde trim", range);
        range = range.replace(re[t.CARETTRIM], caretTrimReplace);
        debug("caret trim", range);
        let rangeList = range.split(" ").map((comp) => parseComparator(comp, this.options)).join(" ").split(/\s+/).map((comp) => replaceGTE0(comp, this.options));
        if (loose) {
          rangeList = rangeList.filter((comp) => {
            debug("loose invalid filter", comp, this.options);
            return !!comp.match(re[t.COMPARATORLOOSE]);
          });
        }
        debug("range list", rangeList);
        const rangeMap = /* @__PURE__ */ new Map();
        const comparators = rangeList.map((comp) => new Comparator(comp, this.options));
        for (const comp of comparators) {
          if (isNullSet(comp)) {
            return [comp];
          }
          rangeMap.set(comp.value, comp);
        }
        if (rangeMap.size > 1 && rangeMap.has("")) {
          rangeMap.delete("");
        }
        const result = [...rangeMap.values()];
        cache.set(memoKey, result);
        return result;
      }
      intersects(range, options) {
        if (!(range instanceof _Range)) {
          throw new TypeError("a Range is required");
        }
        return this.set.some((thisComparators) => {
          return isSatisfiable(thisComparators, options) && range.set.some((rangeComparators) => {
            return isSatisfiable(rangeComparators, options) && thisComparators.every((thisComparator) => {
              return rangeComparators.every((rangeComparator) => {
                return thisComparator.intersects(rangeComparator, options);
              });
            });
          });
        });
      }
      // if ANY of the sets match ALL of its comparators, then pass
      test(version) {
        if (!version) {
          return false;
        }
        if (typeof version === "string") {
          try {
            version = new SemVer(version, this.options);
          } catch (er) {
            return false;
          }
        }
        for (let i = 0; i < this.set.length; i++) {
          if (testSet(this.set[i], version, this.options)) {
            return true;
          }
        }
        return false;
      }
    };
    module2.exports = Range2;
    var LRU = require_lrucache();
    var cache = new LRU();
    var parseOptions = require_parse_options();
    var Comparator = require_comparator();
    var debug = require_debug();
    var SemVer = require_semver();
    var {
      safeRe: re,
      src,
      t,
      comparatorTrimReplace,
      tildeTrimReplace,
      caretTrimReplace
    } = require_re();
    var { FLAG_INCLUDE_PRERELEASE, FLAG_LOOSE } = require_constants();
    var BUILDSTRIPRE = new RegExp(src[t.BUILD], "g");
    var isNullSet = (c) => c.value === "<0.0.0-0";
    var isAny = (c) => c.value === "";
    var isSatisfiable = (comparators, options) => {
      let result = true;
      const remainingComparators = comparators.slice();
      let testComparator = remainingComparators.pop();
      while (result && remainingComparators.length) {
        result = remainingComparators.every((otherComparator) => {
          return testComparator.intersects(otherComparator, options);
        });
        testComparator = remainingComparators.pop();
      }
      return result;
    };
    var parseComparator = (comp, options) => {
      comp = comp.replace(re[t.BUILD], "");
      debug("comp", comp, options);
      comp = replaceCarets(comp, options);
      debug("caret", comp);
      comp = replaceTildes(comp, options);
      debug("tildes", comp);
      comp = replaceXRanges(comp, options);
      debug("xrange", comp);
      comp = replaceStars(comp, options);
      debug("stars", comp);
      return comp;
    };
    var isX = (id) => !id || id.toLowerCase() === "x" || id === "*";
    var invalidXRangeOrder = (M, m, p) => isX(M) && !isX(m) || isX(m) && p && !isX(p);
    var replaceTildes = (comp, options) => {
      return comp.trim().split(/\s+/).map((c) => replaceTilde(c, options)).join(" ");
    };
    var replaceTilde = (comp, options) => {
      const r = options.loose ? re[t.TILDELOOSE] : re[t.TILDE];
      const z = options.includePrerelease ? "-0" : "";
      return comp.replace(r, (_, M, m, p, pr) => {
        debug("tilde", comp, _, M, m, p, pr);
        let ret;
        if (isX(M)) {
          ret = "";
        } else if (isX(m)) {
          ret = `>=${M}.0.0${z} <${+M + 1}.0.0-0`;
        } else if (isX(p)) {
          ret = `>=${M}.${m}.0${z} <${M}.${+m + 1}.0-0`;
        } else if (pr) {
          debug("replaceTilde pr", pr);
          ret = `>=${M}.${m}.${p}-${pr} <${M}.${+m + 1}.0-0`;
        } else {
          ret = `>=${M}.${m}.${p} <${M}.${+m + 1}.0-0`;
        }
        debug("tilde return", ret);
        return ret;
      });
    };
    var replaceCarets = (comp, options) => {
      return comp.trim().split(/\s+/).map((c) => replaceCaret(c, options)).join(" ");
    };
    var replaceCaret = (comp, options) => {
      debug("caret", comp, options);
      const r = options.loose ? re[t.CARETLOOSE] : re[t.CARET];
      const z = options.includePrerelease ? "-0" : "";
      return comp.replace(r, (_, M, m, p, pr) => {
        debug("caret", comp, _, M, m, p, pr);
        let ret;
        if (isX(M)) {
          ret = "";
        } else if (isX(m)) {
          ret = `>=${M}.0.0${z} <${+M + 1}.0.0-0`;
        } else if (isX(p)) {
          if (M === "0") {
            ret = `>=${M}.${m}.0${z} <${M}.${+m + 1}.0-0`;
          } else {
            ret = `>=${M}.${m}.0${z} <${+M + 1}.0.0-0`;
          }
        } else if (pr) {
          debug("replaceCaret pr", pr);
          if (M === "0") {
            if (m === "0") {
              ret = `>=${M}.${m}.${p}-${pr} <${M}.${m}.${+p + 1}-0`;
            } else {
              ret = `>=${M}.${m}.${p}-${pr} <${M}.${+m + 1}.0-0`;
            }
          } else {
            ret = `>=${M}.${m}.${p}-${pr} <${+M + 1}.0.0-0`;
          }
        } else {
          debug("no pr");
          if (M === "0") {
            if (m === "0") {
              ret = `>=${M}.${m}.${p} <${M}.${m}.${+p + 1}-0`;
            } else {
              ret = `>=${M}.${m}.${p} <${M}.${+m + 1}.0-0`;
            }
          } else {
            ret = `>=${M}.${m}.${p} <${+M + 1}.0.0-0`;
          }
        }
        debug("caret return", ret);
        return ret;
      });
    };
    var replaceXRanges = (comp, options) => {
      debug("replaceXRanges", comp, options);
      return comp.split(/\s+/).map((c) => replaceXRange(c, options)).join(" ");
    };
    var replaceXRange = (comp, options) => {
      comp = comp.trim();
      const r = options.loose ? re[t.XRANGELOOSE] : re[t.XRANGE];
      return comp.replace(r, (ret, gtlt, M, m, p, pr) => {
        debug("xRange", comp, ret, gtlt, M, m, p, pr);
        if (invalidXRangeOrder(M, m, p)) {
          return comp;
        }
        const xM = isX(M);
        const xm = xM || isX(m);
        const xp = xm || isX(p);
        const anyX = xp;
        if (gtlt === "=" && anyX) {
          gtlt = "";
        }
        pr = options.includePrerelease ? "-0" : "";
        if (xM) {
          if (gtlt === ">" || gtlt === "<") {
            ret = "<0.0.0-0";
          } else {
            ret = "*";
          }
        } else if (gtlt && anyX) {
          if (xm) {
            m = 0;
          }
          p = 0;
          if (gtlt === ">") {
            gtlt = ">=";
            if (xm) {
              M = +M + 1;
              m = 0;
              p = 0;
            } else {
              m = +m + 1;
              p = 0;
            }
          } else if (gtlt === "<=") {
            gtlt = "<";
            if (xm) {
              M = +M + 1;
            } else {
              m = +m + 1;
            }
          }
          if (gtlt === "<") {
            pr = "-0";
          }
          ret = `${gtlt + M}.${m}.${p}${pr}`;
        } else if (xm) {
          ret = `>=${M}.0.0${pr} <${+M + 1}.0.0-0`;
        } else if (xp) {
          ret = `>=${M}.${m}.0${pr} <${M}.${+m + 1}.0-0`;
        }
        debug("xRange return", ret);
        return ret;
      });
    };
    var replaceStars = (comp, options) => {
      debug("replaceStars", comp, options);
      return comp.trim().replace(re[t.STAR], "");
    };
    var replaceGTE0 = (comp, options) => {
      debug("replaceGTE0", comp, options);
      return comp.trim().replace(re[options.includePrerelease ? t.GTE0PRE : t.GTE0], "");
    };
    var hyphenReplace = (incPr) => ($0, from, fM, fm, fp, fpr, fb, to, tM, tm, tp, tpr) => {
      if (isX(fM)) {
        from = "";
      } else if (isX(fm)) {
        from = `>=${fM}.0.0${incPr ? "-0" : ""}`;
      } else if (isX(fp)) {
        from = `>=${fM}.${fm}.0${incPr ? "-0" : ""}`;
      } else if (fpr) {
        from = `>=${from}`;
      } else {
        from = `>=${from}${incPr ? "-0" : ""}`;
      }
      if (isX(tM)) {
        to = "";
      } else if (isX(tm)) {
        to = `<${+tM + 1}.0.0-0`;
      } else if (isX(tp)) {
        to = `<${tM}.${+tm + 1}.0-0`;
      } else if (tpr) {
        to = `<=${tM}.${tm}.${tp}-${tpr}`;
      } else if (incPr) {
        to = `<${tM}.${tm}.${+tp + 1}-0`;
      } else {
        to = `<=${to}`;
      }
      return `${from} ${to}`.trim();
    };
    var testSet = (set, version, options) => {
      for (let i = 0; i < set.length; i++) {
        if (!set[i].test(version)) {
          return false;
        }
      }
      if (version.prerelease.length && !options.includePrerelease) {
        for (let i = 0; i < set.length; i++) {
          debug(set[i].semver);
          if (set[i].semver === Comparator.ANY) {
            continue;
          }
          if (set[i].semver.prerelease.length > 0) {
            const allowed = set[i].semver;
            if (allowed.major === version.major && allowed.minor === version.minor && allowed.patch === version.patch) {
              return true;
            }
          }
        }
        return false;
      }
      return true;
    };
  }
});

// node_modules/semver/functions/satisfies.js
var require_satisfies = __commonJS({
  "node_modules/semver/functions/satisfies.js"(exports2, module2) {
    "use strict";
    var Range2 = require_range();
    var satisfies = (version, range, options) => {
      try {
        range = new Range2(range, options);
      } catch (er) {
        return false;
      }
      return range.test(version);
    };
    module2.exports = satisfies;
  }
});

// node_modules/vscode-languageclient/lib/common/api.js
var require_api3 = __commonJS({
  "node_modules/vscode-languageclient/lib/common/api.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __exportStar = exports2 && exports2.__exportStar || function(m, exports3) {
      for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports3, p)) __createBinding(exports3, m, p);
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.DiagnosticPullMode = exports2.vsdiag = void 0;
    __exportStar(require_api2(), exports2);
    __exportStar(require_features(), exports2);
    var diagnostic_1 = require_diagnostic();
    Object.defineProperty(exports2, "vsdiag", { enumerable: true, get: function() {
      return diagnostic_1.vsdiag;
    } });
    Object.defineProperty(exports2, "DiagnosticPullMode", { enumerable: true, get: function() {
      return diagnostic_1.DiagnosticPullMode;
    } });
    __exportStar(require_client(), exports2);
  }
});

// node_modules/vscode-languageclient/lib/node/main.js
var require_main3 = __commonJS({
  "node_modules/vscode-languageclient/lib/node/main.js"(exports2) {
    "use strict";
    var __createBinding = exports2 && exports2.__createBinding || (Object.create ? (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      var desc = Object.getOwnPropertyDescriptor(m, k);
      if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
        desc = { enumerable: true, get: function() {
          return m[k];
        } };
      }
      Object.defineProperty(o, k2, desc);
    }) : (function(o, m, k, k2) {
      if (k2 === void 0) k2 = k;
      o[k2] = m[k];
    }));
    var __setModuleDefault = exports2 && exports2.__setModuleDefault || (Object.create ? (function(o, v) {
      Object.defineProperty(o, "default", { enumerable: true, value: v });
    }) : function(o, v) {
      o["default"] = v;
    });
    var __importStar = exports2 && exports2.__importStar || /* @__PURE__ */ (function() {
      var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function(o2) {
          var ar = [];
          for (var k in o2) if (Object.prototype.hasOwnProperty.call(o2, k)) ar[ar.length] = k;
          return ar;
        };
        return ownKeys(o);
      };
      return function(mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) {
          for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        }
        __setModuleDefault(result, mod);
        return result;
      };
    })();
    var __exportStar = exports2 && exports2.__exportStar || function(m, exports3) {
      for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports3, p)) __createBinding(exports3, m, p);
    };
    Object.defineProperty(exports2, "__esModule", { value: true });
    exports2.SettingMonitor = exports2.LanguageClient = exports2.TransportKind = void 0;
    var cp = __importStar(require("child_process"));
    var fs = __importStar(require("fs"));
    var path = __importStar(require("path"));
    var readline = __importStar(require("readline"));
    var vscode_1 = require("vscode");
    var Is2 = __importStar(require_is());
    var client_1 = require_client();
    var processes_1 = require_processes();
    var node_1 = require_main2();
    var semverParse = require_parse();
    var semverSatisfies = require_satisfies();
    __exportStar(require_main2(), exports2);
    __exportStar(require_api3(), exports2);
    var REQUIRED_VSCODE_VERSION = "^1.91.0";
    var TransportKind;
    (function(TransportKind2) {
      TransportKind2[TransportKind2["stdio"] = 0] = "stdio";
      TransportKind2[TransportKind2["ipc"] = 1] = "ipc";
      TransportKind2[TransportKind2["pipe"] = 2] = "pipe";
      TransportKind2[TransportKind2["socket"] = 3] = "socket";
    })(TransportKind || (exports2.TransportKind = TransportKind = {}));
    var Transport;
    (function(Transport2) {
      function isSocket(value) {
        const candidate = value;
        return candidate && candidate.kind === TransportKind.socket && Is2.number(candidate.port);
      }
      Transport2.isSocket = isSocket;
    })(Transport || (Transport = {}));
    var Executable;
    (function(Executable2) {
      function is(value) {
        return Is2.string(value.command);
      }
      Executable2.is = is;
    })(Executable || (Executable = {}));
    var NodeModule;
    (function(NodeModule2) {
      function is(value) {
        return Is2.string(value.module);
      }
      NodeModule2.is = is;
    })(NodeModule || (NodeModule = {}));
    var StreamInfo;
    (function(StreamInfo2) {
      function is(value) {
        const candidate = value;
        return candidate && candidate.writer !== void 0 && candidate.reader !== void 0;
      }
      StreamInfo2.is = is;
    })(StreamInfo || (StreamInfo = {}));
    var ChildProcessInfo;
    (function(ChildProcessInfo2) {
      function is(value) {
        const candidate = value;
        return candidate && candidate.process !== void 0 && typeof candidate.detached === "boolean";
      }
      ChildProcessInfo2.is = is;
    })(ChildProcessInfo || (ChildProcessInfo = {}));
    var LanguageClient2 = class extends client_1.BaseLanguageClient {
      _serverOptions;
      _forceDebug;
      _serverProcess;
      _isDetached;
      _isInDebugMode;
      constructor(arg1, arg2, arg3, arg4, arg5) {
        let id;
        let name;
        let serverOptions;
        let clientOptions;
        let forceDebug;
        if (Is2.string(arg2)) {
          id = arg1;
          name = arg2;
          serverOptions = arg3;
          clientOptions = arg4;
          forceDebug = !!arg5;
        } else {
          id = arg1.toLowerCase();
          name = arg1;
          serverOptions = arg2;
          clientOptions = arg3;
          forceDebug = arg4;
        }
        if (forceDebug === void 0) {
          forceDebug = false;
        }
        super(id, name, clientOptions);
        this._serverOptions = serverOptions;
        this._forceDebug = forceDebug;
        this._isInDebugMode = forceDebug;
        try {
          this.checkVersion();
        } catch (error) {
          if (Is2.string(error.message)) {
            this.outputChannel.appendLine(error.message);
          }
          throw error;
        }
      }
      checkVersion() {
        const codeVersion = semverParse(vscode_1.version);
        if (!codeVersion) {
          throw new Error(`No valid VS Code version detected. Version string is: ${vscode_1.version}`);
        }
        if (codeVersion.prerelease && codeVersion.prerelease.length > 0) {
          codeVersion.prerelease = [];
        }
        if (!semverSatisfies(codeVersion, REQUIRED_VSCODE_VERSION)) {
          throw new Error(`The language client requires VS Code version ${REQUIRED_VSCODE_VERSION} but received version ${vscode_1.version}`);
        }
      }
      get isInDebugMode() {
        return this._isInDebugMode;
      }
      get serverProcess() {
        return this._serverProcess;
      }
      async restart() {
        await this.stop();
        if (this.isInDebugMode) {
          await new Promise((resolve) => setTimeout(resolve, 1e3));
          await this.start();
        } else {
          await this.start();
        }
      }
      shutdown(mode, timeout = 2e3) {
        return super.shutdown(mode, timeout).finally(() => {
          if (this._serverProcess) {
            const toCheck = this._serverProcess;
            this._serverProcess = void 0;
            if (this._isDetached === void 0 || !this._isDetached) {
              this.checkProcessDied(toCheck);
            }
            this._isDetached = void 0;
          }
        });
      }
      checkProcessDied(childProcess) {
        if (!childProcess || childProcess.pid === void 0) {
          return;
        }
        setTimeout(() => {
          try {
            if (childProcess.pid !== void 0) {
              process.kill(childProcess.pid, 0);
              (0, processes_1.terminate)(childProcess);
            }
          } catch (error) {
          }
        }, 2e3);
      }
      handleConnectionClosed() {
        this._serverProcess = void 0;
        return super.handleConnectionClosed();
      }
      fillInitializeParams(params) {
        super.fillInitializeParams(params);
        if (params.processId === null) {
          params.processId = process.pid;
        }
      }
      createMessageTransports(_encoding) {
        function getEnvironment(env2, fork) {
          if (!env2 && !fork) {
            return void 0;
          }
          const result = /* @__PURE__ */ Object.create(null);
          Object.keys(process.env).forEach((key) => result[key] = process.env[key]);
          if (fork) {
            result["ELECTRON_RUN_AS_NODE"] = "1";
            result["ELECTRON_NO_ASAR"] = "1";
          }
          if (env2) {
            Object.keys(env2).forEach((key) => result[key] = env2[key]);
          }
          return result;
        }
        const debugStartWith = ["--debug=", "--debug-brk=", "--inspect=", "--inspect-brk="];
        const debugEquals = ["--debug", "--debug-brk", "--inspect", "--inspect-brk"];
        function startedInDebugMode() {
          const args = process.execArgv;
          if (args) {
            return args.some((arg) => {
              return debugStartWith.some((value) => arg.startsWith(value)) || debugEquals.some((value) => arg === value);
            });
          }
          return false;
        }
        function assertStdio(process2) {
          if (process2.stdin === null || process2.stdout === null || process2.stderr === null) {
            throw new Error("Process created without stdio streams");
          }
        }
        function pipeStdoutToLogOutputChannel(input, outputChannel) {
          readline.createInterface({
            input,
            crlfDelay: Infinity,
            terminal: false,
            historySize: 0
          }).on("line", (data) => outputChannel.info(data));
        }
        function pipeStderrToLogOutputChannel(input, outputChannel) {
          readline.createInterface({
            input,
            crlfDelay: Infinity,
            terminal: false,
            historySize: 0
          }).on("line", (data) => outputChannel.error(data));
        }
        const server = this._serverOptions;
        if (Is2.func(server)) {
          return server().then((result) => {
            if (client_1.MessageTransports.is(result)) {
              this._isDetached = !!result.detached;
              return result;
            } else if (StreamInfo.is(result)) {
              this._isDetached = !!result.detached;
              return { reader: new node_1.StreamMessageReader(result.reader), writer: new node_1.StreamMessageWriter(result.writer) };
            } else {
              let cp2;
              if (ChildProcessInfo.is(result)) {
                cp2 = result.process;
                this._isDetached = result.detached;
              } else {
                cp2 = result;
                this._isDetached = false;
              }
              pipeStderrToLogOutputChannel(cp2.stderr, this.outputChannel);
              return { reader: new node_1.StreamMessageReader(cp2.stdout), writer: new node_1.StreamMessageWriter(cp2.stdin) };
            }
          });
        }
        let json;
        const runDebug = server;
        if (runDebug.run || runDebug.debug) {
          if (this._forceDebug || startedInDebugMode()) {
            json = runDebug.debug;
            this._isInDebugMode = true;
          } else {
            json = runDebug.run;
            this._isInDebugMode = false;
          }
        } else {
          json = server;
        }
        return this._getServerWorkingDir(json.options).then((serverWorkingDir) => {
          if (NodeModule.is(json) && json.module) {
            const node = json;
            const transport = node.transport || TransportKind.stdio;
            if (node.runtime) {
              const args = [];
              const options = node.options ?? /* @__PURE__ */ Object.create(null);
              if (options.execArgv) {
                options.execArgv.forEach((element) => args.push(element));
              }
              args.push(node.module);
              if (node.args) {
                node.args.forEach((element) => args.push(element));
              }
              const execOptions = /* @__PURE__ */ Object.create(null);
              execOptions.cwd = serverWorkingDir;
              execOptions.env = getEnvironment(options.env, false);
              const runtime = this._getRuntimePath(node.runtime, serverWorkingDir);
              let pipeName = void 0;
              if (transport === TransportKind.ipc) {
                execOptions.stdio = [null, null, null, "ipc"];
                args.push("--node-ipc");
              } else if (transport === TransportKind.stdio) {
                args.push("--stdio");
              } else if (transport === TransportKind.pipe) {
                pipeName = (0, node_1.generateRandomPipeName)();
                args.push(`--pipe=${pipeName}`);
              } else if (Transport.isSocket(transport)) {
                args.push(`--socket=${transport.port}`);
              }
              args.push(`--clientProcessId=${process.pid.toString()}`);
              if (transport === TransportKind.ipc || transport === TransportKind.stdio) {
                const serverProcess = cp.spawn(runtime, args, execOptions);
                if (!serverProcess || !serverProcess.pid) {
                  return handleChildProcessStartError(serverProcess, `Launching server using runtime ${runtime} failed.`);
                }
                this._serverProcess = serverProcess;
                pipeStderrToLogOutputChannel(serverProcess.stderr, this.outputChannel);
                if (transport === TransportKind.ipc) {
                  pipeStdoutToLogOutputChannel(serverProcess.stdout, this.outputChannel);
                  return Promise.resolve({ reader: new node_1.IPCMessageReader(serverProcess), writer: new node_1.IPCMessageWriter(serverProcess) });
                } else {
                  return Promise.resolve({ reader: new node_1.StreamMessageReader(serverProcess.stdout), writer: new node_1.StreamMessageWriter(serverProcess.stdin) });
                }
              } else if (transport === TransportKind.pipe) {
                return (0, node_1.createClientPipeTransport)(pipeName).then((transport2) => {
                  const process2 = cp.spawn(runtime, args, execOptions);
                  if (!process2 || !process2.pid) {
                    return handleChildProcessStartError(process2, `Launching server using runtime ${runtime} failed.`);
                  }
                  this._serverProcess = process2;
                  pipeStderrToLogOutputChannel(process2.stderr, this.outputChannel);
                  pipeStdoutToLogOutputChannel(process2.stdout, this.outputChannel);
                  return transport2.onConnected().then((protocol) => {
                    return { reader: protocol[0], writer: protocol[1] };
                  });
                });
              } else if (Transport.isSocket(transport)) {
                return (0, node_1.createClientSocketTransport)(transport.port).then((transport2) => {
                  const process2 = cp.spawn(runtime, args, execOptions);
                  if (!process2 || !process2.pid) {
                    return handleChildProcessStartError(process2, `Launching server using runtime ${runtime} failed.`);
                  }
                  this._serverProcess = process2;
                  pipeStderrToLogOutputChannel(process2.stderr, this.outputChannel);
                  pipeStdoutToLogOutputChannel(process2.stdout, this.outputChannel);
                  return transport2.onConnected().then((protocol) => {
                    return { reader: protocol[0], writer: protocol[1] };
                  });
                });
              }
            } else {
              let pipeName = void 0;
              return new Promise((resolve, reject) => {
                const args = (node.args && node.args.slice()) ?? [];
                if (transport === TransportKind.ipc) {
                  args.push("--node-ipc");
                } else if (transport === TransportKind.stdio) {
                  args.push("--stdio");
                } else if (transport === TransportKind.pipe) {
                  pipeName = (0, node_1.generateRandomPipeName)();
                  args.push(`--pipe=${pipeName}`);
                } else if (Transport.isSocket(transport)) {
                  args.push(`--socket=${transport.port}`);
                }
                args.push(`--clientProcessId=${process.pid.toString()}`);
                const options = node.options ? { ...node.options } : /* @__PURE__ */ Object.create(null);
                options.env = getEnvironment(options.env, true);
                options.execArgv = options.execArgv || [];
                options.cwd = serverWorkingDir;
                options.silent = true;
                if (transport === TransportKind.ipc || transport === TransportKind.stdio) {
                  const sp = cp.fork(node.module, args || [], options);
                  assertStdio(sp);
                  this._serverProcess = sp;
                  pipeStderrToLogOutputChannel(sp.stderr, this.outputChannel);
                  if (transport === TransportKind.ipc) {
                    pipeStdoutToLogOutputChannel(sp.stdout, this.outputChannel);
                    resolve({ reader: new node_1.IPCMessageReader(this._serverProcess), writer: new node_1.IPCMessageWriter(this._serverProcess) });
                  } else {
                    resolve({ reader: new node_1.StreamMessageReader(sp.stdout), writer: new node_1.StreamMessageWriter(sp.stdin) });
                  }
                } else if (transport === TransportKind.pipe) {
                  (0, node_1.createClientPipeTransport)(pipeName).then((transport2) => {
                    const sp = cp.fork(node.module, args || [], options);
                    assertStdio(sp);
                    this._serverProcess = sp;
                    pipeStderrToLogOutputChannel(sp.stderr, this.outputChannel);
                    pipeStdoutToLogOutputChannel(sp.stdout, this.outputChannel);
                    transport2.onConnected().then((protocol) => {
                      resolve({ reader: protocol[0], writer: protocol[1] });
                    }, reject);
                  }, reject);
                } else if (Transport.isSocket(transport)) {
                  (0, node_1.createClientSocketTransport)(transport.port).then((transport2) => {
                    const sp = cp.fork(node.module, args || [], options);
                    assertStdio(sp);
                    this._serverProcess = sp;
                    pipeStderrToLogOutputChannel(sp.stderr, this.outputChannel);
                    pipeStdoutToLogOutputChannel(sp.stdout, this.outputChannel);
                    transport2.onConnected().then((protocol) => {
                      resolve({ reader: protocol[0], writer: protocol[1] });
                    }, reject);
                  }, reject);
                }
              });
            }
          } else if (Executable.is(json) && json.command) {
            const command = json;
            const args = json.args !== void 0 ? json.args.slice(0) : [];
            let pipeName = void 0;
            const transport = json.transport;
            if (transport === TransportKind.stdio) {
              args.push("--stdio");
            } else if (transport === TransportKind.pipe) {
              pipeName = (0, node_1.generateRandomPipeName)();
              args.push(`--pipe=${pipeName}`);
            } else if (Transport.isSocket(transport)) {
              args.push(`--socket=${transport.port}`);
            } else if (transport === TransportKind.ipc) {
              throw new Error(`Transport kind ipc is not support for command executable`);
            }
            const options = Object.assign({}, command.options);
            options.cwd = options.cwd || serverWorkingDir;
            if (transport === void 0 || transport === TransportKind.stdio) {
              const serverProcess = cp.spawn(command.command, args, options);
              if (!serverProcess || !serverProcess.pid) {
                return handleChildProcessStartError(serverProcess, `Launching server using command ${command.command} failed.`);
              }
              pipeStderrToLogOutputChannel(serverProcess.stderr, this.outputChannel);
              this._serverProcess = serverProcess;
              this._isDetached = !!options.detached;
              return Promise.resolve({ reader: new node_1.StreamMessageReader(serverProcess.stdout), writer: new node_1.StreamMessageWriter(serverProcess.stdin) });
            } else if (transport === TransportKind.pipe) {
              return (0, node_1.createClientPipeTransport)(pipeName).then((transport2) => {
                const serverProcess = cp.spawn(command.command, args, options);
                if (!serverProcess || !serverProcess.pid) {
                  return handleChildProcessStartError(serverProcess, `Launching server using command ${command.command} failed.`);
                }
                this._serverProcess = serverProcess;
                this._isDetached = !!options.detached;
                pipeStderrToLogOutputChannel(serverProcess.stderr, this.outputChannel);
                pipeStdoutToLogOutputChannel(serverProcess.stdout, this.outputChannel);
                return transport2.onConnected().then((protocol) => {
                  return { reader: protocol[0], writer: protocol[1] };
                });
              });
            } else if (Transport.isSocket(transport)) {
              return (0, node_1.createClientSocketTransport)(transport.port).then((transport2) => {
                const serverProcess = cp.spawn(command.command, args, options);
                if (!serverProcess || !serverProcess.pid) {
                  return handleChildProcessStartError(serverProcess, `Launching server using command ${command.command} failed.`);
                }
                this._serverProcess = serverProcess;
                this._isDetached = !!options.detached;
                pipeStderrToLogOutputChannel(serverProcess.stderr, this.outputChannel);
                pipeStdoutToLogOutputChannel(serverProcess.stdout, this.outputChannel);
                return transport2.onConnected().then((protocol) => {
                  return { reader: protocol[0], writer: protocol[1] };
                });
              });
            }
          }
          return Promise.reject(new Error(`Unsupported server configuration ` + JSON.stringify(server, null, 4)));
        }).finally(() => {
          if (this._serverProcess !== void 0) {
            this._serverProcess.on("exit", (code, signal) => {
              if (code === 0) {
                this.info("Server process exited successfully", void 0, false);
              } else if (code !== null) {
                this.error(`Server process exited with code ${code}.`, void 0, false);
              }
              if (signal !== null) {
                this.error(`Server process exited with signal ${signal}.`, void 0, false);
              }
            });
          }
        });
      }
      _getRuntimePath(runtime, serverWorkingDirectory) {
        if (path.isAbsolute(runtime)) {
          return runtime;
        }
        const mainRootPath = this._mainGetRootPath();
        if (mainRootPath !== void 0) {
          const result = path.join(mainRootPath, runtime);
          if (fs.existsSync(result)) {
            return result;
          }
        }
        if (serverWorkingDirectory !== void 0) {
          const result = path.join(serverWorkingDirectory, runtime);
          if (fs.existsSync(result)) {
            return result;
          }
        }
        return runtime;
      }
      _mainGetRootPath() {
        const folders = vscode_1.workspace.workspaceFolders;
        if (!folders || folders.length === 0) {
          return void 0;
        }
        const folder = folders[0];
        if (folder.uri.scheme === "file") {
          return folder.uri.fsPath;
        }
        return void 0;
      }
      _getServerWorkingDir(options) {
        let cwd = options && options.cwd;
        if (!cwd) {
          cwd = this.clientOptions.workspaceFolder ? this.clientOptions.workspaceFolder.uri.fsPath : this._mainGetRootPath();
        }
        if (cwd) {
          return new Promise((s) => {
            fs.lstat(cwd, (err, stats) => {
              s(!err && stats.isDirectory() ? cwd : void 0);
            });
          });
        }
        return Promise.resolve(void 0);
      }
    };
    exports2.LanguageClient = LanguageClient2;
    var SettingMonitor = class {
      _client;
      _setting;
      _listeners;
      constructor(_client, _setting) {
        this._client = _client;
        this._setting = _setting;
        this._listeners = [];
      }
      start() {
        vscode_1.workspace.onDidChangeConfiguration(this.onDidChangeConfiguration, this, this._listeners);
        this.onDidChangeConfiguration();
        return new vscode_1.Disposable(() => {
          if (this._client.needsStop()) {
            void this._client.stop();
          }
        });
      }
      onDidChangeConfiguration() {
        const index = this._setting.indexOf(".");
        const primary = index >= 0 ? this._setting.substr(0, index) : this._setting;
        const rest = index >= 0 ? this._setting.substr(index + 1) : void 0;
        const enabled = rest ? vscode_1.workspace.getConfiguration(primary).get(rest, false) : vscode_1.workspace.getConfiguration(primary);
        if (enabled && this._client.needsStart()) {
          this._client.start().catch((error) => this._client.error("Start failed after configuration change", error, "force"));
        } else if (!enabled && this._client.needsStop()) {
          void this._client.stop().catch((error) => this._client.error("Stop failed after configuration change", error, "force"));
        }
      }
    };
    exports2.SettingMonitor = SettingMonitor;
    function handleChildProcessStartError(process2, message) {
      if (process2 === null) {
        return Promise.reject(message);
      }
      return new Promise((_, reject) => {
        process2.on("error", (err) => {
          reject(`${message} ${err}`);
        });
        setImmediate(() => reject(message));
      });
    }
  }
});

// src/extension.ts
var extension_exports = {};
__export(extension_exports, {
  activate: () => activate,
  deactivate: () => deactivate
});
module.exports = __toCommonJS(extension_exports);
var vscode = __toESM(require("vscode"));
var import_node = __toESM(require_main3());

// src/nativeDiagnostics.ts
var MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT = 100;
var MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES = 512;
var MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT = 32;
var MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES = 256;
var SAFE_DIAGNOSTIC_DATA_FIELDS = ["category", "href", "kind", "ruleId", "url"];
var PACKAGED_TRACEDECAY_BINARY = "__TRACEDECAY_BIN__";
function admitNativeWorkspace(trusted, roots) {
  if (!trusted) {
    return {
      state: "unavailable",
      reason: "workspace_untrusted",
      expectedRootCount: 1,
      actualRootCount: roots.length
    };
  }
  if (roots.length !== 1) {
    return {
      state: "unavailable",
      reason: "workspace_root_count",
      expectedRootCount: 1,
      actualRootCount: roots.length
    };
  }
  return { state: "supported", root: roots[0] };
}
function traceDecayInitializationOptions() {
  return {
    tracedecay: {
      context: true,
      mode: "cursor-native"
    }
  };
}
function toLspDiagnosticSeverity(vscodeSeverity) {
  switch (vscodeSeverity) {
    case 0:
      return 1;
    case 1:
      return 2;
    case 2:
      return 3;
    case 3:
      return 4;
    default:
      return void 0;
  }
}
function resolveInstalledTraceDecayBinary(configuredBinary, environmentBinary, packagedBinary = PACKAGED_TRACEDECAY_BINARY) {
  for (const candidate of [
    configuredBinary,
    environmentBinary,
    packagedBinary.startsWith("__TRACEDECAY_") ? void 0 : packagedBinary
  ]) {
    const binary = candidate?.trim();
    if (binary !== void 0 && binary.length > 0) {
      return binary;
    }
  }
  return "tracedecay";
}
function limitAdmittedNativeDiagnosticDocuments(documents, isAdmitted) {
  return documents.filter(isAdmitted).slice(0, MAX_NATIVE_DIAGNOSTIC_DOCUMENTS_PER_EVENT);
}
function createNativeDiagnosticsPayload(uri, version, diagnostics) {
  return {
    uri,
    version: Number.isSafeInteger(version) ? version : 0,
    diagnostics: diagnostics.slice(0, MAX_NATIVE_DIAGNOSTICS_PER_DOCUMENT).map((diagnostic) => {
      const code = diagnostic.code === void 0 ? void 0 : truncateUtf8(
        String(diagnostic.code),
        MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES
      );
      const message = truncateUtf8(diagnostic.message, MAX_NATIVE_DIAGNOSTIC_MESSAGE_BYTES) || "Cursor diagnostic";
      return {
        range: diagnostic.range,
        ...diagnostic.severity === void 0 ? {} : { severity: diagnostic.severity },
        ...code === void 0 || code.length === 0 ? {} : { code },
        source: truncateUtf8(
          diagnostic.source || "cursor",
          MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES
        ),
        message,
        data: sanitizeDiagnosticData(diagnostic.data)
      };
    })
  };
}
function sanitizeDiagnosticData(data) {
  if (!isRecord(data)) {
    return null;
  }
  const safe = {};
  for (const field of SAFE_DIAGNOSTIC_DATA_FIELDS) {
    const value = data[field];
    if (typeof value === "boolean") {
      safe[field] = value;
    } else if (typeof value === "number" && Number.isFinite(value)) {
      safe[field] = value;
    } else if (typeof value === "string") {
      safe[field] = truncateUtf8(value, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES);
    }
  }
  return Object.keys(safe).length === 0 ? null : safe;
}
function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function truncateUtf8(value, maximumBytes) {
  if (Buffer.byteLength(value, "utf8") <= maximumBytes) {
    return value;
  }
  let result = "";
  let byteLength = 0;
  for (const character of value) {
    const characterBytes = Buffer.byteLength(character, "utf8");
    if (byteLength + characterBytes > maximumBytes) {
      break;
    }
    result += character;
    byteLength += characterBytes;
  }
  return result;
}

// src/extension.ts
var TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD = "tracedecay/nativeDiagnostics";
var TRACEDECAY_CONTEXT_PROJECTIONS = [
  { kind: "diagnostics", revision: 1 },
  { kind: "postEditImpact", revision: 1 },
  { kind: "affectedTests", revision: 1 },
  { kind: "testRunResults", revision: 1 }
];
var activeController;
async function activate(context) {
  if (!isCursorDesktop()) {
    return;
  }
  const controller = new CursorNativeDiagnosticsController();
  activeController = controller;
  context.subscriptions.push(controller);
  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      void controller.reconcile();
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidGrantWorkspaceTrust(() => {
      void controller.reconcile();
    })
  );
  await controller.reconcile();
}
async function deactivate() {
  await activeController?.stop();
  activeController = void 0;
}
var CursorNativeDiagnosticsController = class {
  disposed = false;
  queue = Promise.resolve();
  session;
  reconcile() {
    return this.enqueue(async () => {
      await this.session?.stop();
      this.session = void 0;
      if (this.disposed) {
        return;
      }
      const admission = admitNativeWorkspace(
        vscode.workspace.isTrusted,
        vscode.workspace.workspaceFolders ?? []
      );
      if (admission.state === "unavailable") {
        if (admission.reason === "workspace_root_count") {
          void vscode.window.showWarningMessage(
            "TraceDecay native diagnostics require exactly one workspace folder; multi-root workspaces are not supported yet."
          );
        }
        return;
      }
      try {
        this.session = await CursorNativeDiagnosticsSession.start(admission.root);
      } catch (error) {
        void vscode.window.showWarningMessage(
          `TraceDecay native diagnostics could not start: ${String(error)}`
        );
      }
    });
  }
  stop() {
    this.disposed = true;
    return this.enqueue(async () => {
      await this.session?.stop();
      this.session = void 0;
    });
  }
  dispose() {
    void this.stop();
  }
  enqueue(operation) {
    this.queue = this.queue.catch(() => void 0).then(operation);
    return this.queue;
  }
};
var CursorNativeDiagnosticsSession = class _CursorNativeDiagnosticsSession {
  constructor(client, workspaceFolder) {
    this.client = client;
    this.workspaceFolder = workspaceFolder;
    this.diagnosticsSubscription = vscode.languages.onDidChangeDiagnostics((event) => {
      this.sendChangedDiagnostics(event.uris);
    });
    this.documentSubscription = vscode.workspace.onDidOpenTextDocument((document) => {
      this.sendChangedDiagnostics([document.uri]);
    });
    this.stateSubscription = client.onDidChangeState((event) => {
      if (event.newState === import_node.State.Running) {
        this.sendCurrentDiagnostics();
      }
    });
  }
  client;
  workspaceFolder;
  disposed = false;
  diagnosticsSubscription;
  documentSubscription;
  stateSubscription;
  static async start(workspaceFolder) {
    const serverOptions = {
      command: resolveTraceDecayBinary(),
      args: ["lsp", "bridge", "--stdio"],
      options: {
        cwd: workspaceFolder.uri.fsPath
      }
    };
    const clientOptions = {
      documentSelector: [
        {
          scheme: "file",
          pattern: `${workspaceFolder.uri.fsPath.replaceAll("\\", "/")}/**`
        }
      ],
      initializationOptions: traceDecayInitializationOptions,
      workspaceFolder
    };
    const client = new TraceDecayLanguageClient(
      "tracedecayCursorNative",
      "TraceDecay Cursor Native Diagnostics",
      serverOptions,
      clientOptions
    );
    await client.start();
    const session = new _CursorNativeDiagnosticsSession(client, workspaceFolder);
    session.sendCurrentDiagnostics();
    return session;
  }
  async stop() {
    await this.disposeAsync();
  }
  dispose() {
    void this.disposeAsync();
  }
  async disposeAsync() {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.diagnosticsSubscription.dispose();
    this.documentSubscription.dispose();
    this.stateSubscription.dispose();
    await this.client.stop();
  }
  sendCurrentDiagnostics() {
    const uris = vscode.languages.getDiagnostics().map(([uri]) => uri);
    this.sendChangedDiagnostics(uris);
  }
  sendChangedDiagnostics(uris) {
    if (this.disposed) {
      return;
    }
    for (const uri of limitAdmittedNativeDiagnosticDocuments(
      uris,
      (candidate) => isAdmittedWorkspaceDocument(candidate, this.workspaceFolder)
    )) {
      const document = vscode.workspace.textDocuments.find(
        (candidate) => candidate.uri.toString() === uri.toString()
      );
      if (document === void 0) {
        continue;
      }
      const payload = createNativeDiagnosticsPayload(
        uri.toString(),
        document.version,
        vscode.languages.getDiagnostics(uri).map(toNativeDiagnosticInput)
      );
      void this.client.sendNotification(TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD, payload).catch((error) => {
        this.client.warn(
          `TraceDecay native diagnostic notification was not delivered: ${String(error)}`
        );
      });
    }
  }
};
var TraceDecayLanguageClient = class extends import_node.LanguageClient {
  fillInitializeParams(params) {
    super.fillInitializeParams(params);
    const capabilities = params.capabilities;
    capabilities.experimental = {
      ...capabilities.experimental,
      tracedecay: {
        opaqueExpansion: true,
        projections: TRACEDECAY_CONTEXT_PROJECTIONS,
        revision: 1
      }
    };
  }
};
function isCursorDesktop() {
  return vscode.env.appName.toLocaleLowerCase().includes("cursor");
}
function resolveTraceDecayBinary() {
  const configured = vscode.workspace.getConfiguration("tracedecay").get("binaryPath")?.trim();
  return resolveInstalledTraceDecayBinary(configured, process.env.TRACEDECAY_BIN);
}
function isAdmittedWorkspaceDocument(uri, workspaceFolder) {
  return uri.scheme === "file" && vscode.workspace.getWorkspaceFolder(uri)?.uri.toString() === workspaceFolder.uri.toString();
}
function toNativeDiagnosticInput(diagnostic) {
  const source = diagnostic.source?.trim();
  const code = diagnostic.code;
  const severity = toLspDiagnosticSeverity(diagnostic.severity);
  return {
    range: {
      start: diagnostic.range.start,
      end: diagnostic.range.end
    },
    ...severity === void 0 ? {} : { severity },
    ...code === void 0 ? {} : { code: diagnosticCode(code) },
    ...source === void 0 || source.length === 0 ? {} : { source },
    message: diagnostic.message,
    data: diagnostic.tags?.length ? { category: diagnostic.tags.join(",") } : void 0
  };
}
function diagnosticCode(code) {
  return typeof code === "object" ? code.value : code;
}
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  activate,
  deactivate
});
