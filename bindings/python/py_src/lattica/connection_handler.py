import inspect
from typing import List, get_type_hints
from .client import Lattica

def is_protobuf_message(obj):
    return hasattr(obj, 'SerializeToString') and hasattr(obj, 'ParseFromString')

def is_protobuf_class(cls):
    # Check if it has protobuf key methods
    has_serialize = hasattr(cls, 'SerializeToString')
    has_parse = hasattr(cls, 'ParseFromString')

    # Real protobuf classes or mock protobuf classes should have both methods
    if has_serialize and has_parse:
        return True

    # Fallback check: module name contains _pb2 (real protobuf generated files)
    if hasattr(cls, '__module__') and '_pb2' in cls.__module__:
        return True

    return False

def smart_serialize(data):
    if data is None:
        return b''
    elif is_protobuf_message(data):
        return data.SerializeToString()
    else:
        import pickle
        return pickle.dumps(data)

def smart_deserialize(data, expected_type=None):
    if not data:
        return None

    # If protobuf type is specified, try protobuf deserialization
    if expected_type and is_protobuf_class(expected_type):
        try:
            proto_obj = expected_type()
            proto_obj.ParseFromString(data)
            return proto_obj
        except Exception:
            pass  # Try fallback methods

    # Try auto-detection - check if it's pickle serialized dict with protobuf characteristics
    try:
        import pickle
        result = pickle.loads(data)

        # If result is dict, try to convert to matching protobuf type
        if isinstance(result, dict) and expected_type and is_protobuf_class(expected_type):
            # Try to create protobuf object and set attributes
            try:
                proto_obj = expected_type()
                for key, value in result.items():
                    if hasattr(proto_obj, key):
                        setattr(proto_obj, key, value)
                return proto_obj
            except Exception:
                pass

        return result
    except Exception as e:
        raise RuntimeError(f"Failed to deserialize data: {e}")

def get_method_type_hints(method):
    try:
        type_hints = get_type_hints(method)
        sig = inspect.signature(method)

        # Analyze parameter types
        param_types = {}
        for param_name, param in sig.parameters.items():
            if param_name in type_hints and param_name != 'self':
                annotation = type_hints[param_name]
                if is_protobuf_class(annotation):
                    param_types[param_name] = annotation
                # Handle generics like Optional[ProtoType]
                elif hasattr(annotation, '__origin__'):
                    for arg in getattr(annotation, '__args__', []):
                        if is_protobuf_class(arg):
                            param_types[param_name] = arg
                            break

        # Analyze return type
        return_type = None
        if 'return' in type_hints:
            annotation = type_hints['return']
            if is_protobuf_class(annotation):
                return_type = annotation
            elif hasattr(annotation, '__origin__'):
                for arg in getattr(annotation, '__args__', []):
                    if is_protobuf_class(arg):
                        return_type = arg
                        break

        return param_types, return_type
    except Exception:
        return {}, None

class ConnectionHandlerMeta(type):
    def __new__(mcs, name, bases, attrs):
        # Collect RPC methods and stream methods
        rpc_methods = set()
        stream_methods = set()
        stream_iter_methods = set()

        # Inherit methods from base classes
        for base in bases:
            if hasattr(base, '_rpc_methods'):
                rpc_methods.update(base._rpc_methods)
            if hasattr(base, '_stream_methods'):
                stream_methods.update(base._stream_methods)
            if hasattr(base, '_stream_iter_methods'):
                stream_iter_methods.update(base._stream_iter_methods)

        # Scan current class methods
        for attr_name, attr_value in attrs.items():
            if hasattr(attr_value, '_is_rpc_method'):
                if getattr(attr_value, '_is_stream_iter_method', False):
                    stream_iter_methods.add(attr_name)
                elif getattr(attr_value, '_is_stream_method', False):
                    stream_methods.add(attr_name)
                else:
                    rpc_methods.add(attr_name)

        attrs['_rpc_methods'] = list(rpc_methods)
        attrs['_stream_methods'] = list(stream_methods)
        attrs['_stream_iter_methods'] = list(stream_iter_methods)

        # Create handlers for RPC methods
        for method_name in rpc_methods:
            if f'_handle_{method_name}' not in attrs:
                attrs[f'_handle_{method_name}'] = mcs._create_rpc_handler(method_name)

        # Create handlers for Stream methods
        for method_name in stream_methods:
            if f'_handle_stream_{method_name}' not in attrs:
                attrs[f'_handle_stream_{method_name}'] = mcs._create_stream_handler(method_name)

        for method_name in stream_iter_methods:
            if f'_handle_stream_{method_name}' not in attrs:
                attrs[f'_handle_stream_iter_{method_name}'] = mcs._create_stream_iter_handler(method_name)

        # Save method type information for client use
        attrs['_method_type_info'] = {}
        for method_name in rpc_methods | stream_methods | stream_iter_methods:
            if method_name in attrs:
                method = attrs[method_name]
                attrs['_method_type_info'][method_name] = get_method_type_hints(method)

        return super().__new__(mcs, name, bases, attrs)

    @staticmethod
    def _create_rpc_handler(method_name: str):
        def handler(self, data: bytes) -> bytes:
            try:
                # Get method and type information
                method = getattr(self, method_name)
                param_types, return_type = get_method_type_hints(method)

                # Smart deserialization of request data
                if data:
                    # Check if there's a single protobuf parameter
                    sig = inspect.signature(method)
                    params = [p for name, p in sig.parameters.items() if name != 'self']

                    if len(params) == 1 and params[0].name in param_types:
                        # Single protobuf parameter
                        proto_type = param_types[params[0].name]
                        request_data = smart_deserialize(data, proto_type)
                    else:
                        # Other cases
                        request_data = smart_deserialize(data)
                else:
                    request_data = None

                # Call method
                result = ConnectionHandlerMeta._call_method(method, request_data)

                # Smart serialization of return data
                return smart_serialize(result)

            except Exception as e:
                raise RuntimeError(f"RPC method {method_name} failed: {e}")
        return handler

    @staticmethod
    def _create_stream_handler(method_name: str):
        def handler(self, data: bytes) ->bytes:
            try:
                # Get method and type information
                method = getattr(self, method_name)
                param_types, return_type = get_method_type_hints(method)

                # Smart deserialization of request data
                if data:
                    sig = inspect.signature(method)
                    params = [p for name, p in sig.parameters.items() if name != 'self']

                    if len(params) == 1 and params[0].name in param_types:
                        # Single protobuf parameter
                        proto_type = param_types[params[0].name]
                        request_data = smart_deserialize(data, proto_type)
                    else:
                        # Other cases
                        request_data = smart_deserialize(data)
                else:
                    request_data = None

                # Call method
                result = ConnectionHandlerMeta._call_method(method, request_data)
                return smart_serialize(result)

            except Exception as e:
                raise RuntimeError(f"Stream method {method_name} failed: {e}")
        return handler

    @staticmethod
    def _create_stream_iter_handler(method_name: str):
        def handler(self, data: bytes) ->bytes:
            try:
                # Get method and type information
                method = getattr(self, method_name)
                param_types, _ = get_method_type_hints(method)

                # Smart deserialization of request data
                if data:
                    sig = inspect.signature(method)
                    params = [p for name, p in sig.parameters.items() if name != 'self']

                    if len(params) == 1 and params[0].name in param_types:
                        # Single protobuf parameter
                        proto_type = param_types[params[0].name]
                        request_data = smart_deserialize(data, proto_type)
                    else:
                        # Other cases
                        request_data = smart_deserialize(data)
                else:
                    request_data = None

                # Call method
                return ConnectionHandlerMeta._call_method(method, request_data)
            except Exception as e:
                raise RuntimeError(f"Stream iter method {method_name} failed: {e}")
        return handler

    @staticmethod
    def _call_method(method, request_data):
        if request_data is not None:
            sig = inspect.signature(method)
            params = list(sig.parameters.values())
            if params and params[0].name == 'self':
                params = params[1:]

            if len(params) == 0:
                return method()
            elif len(params) == 1:
                return method(request_data)
            else:
                if isinstance(request_data, dict):
                    return method(**request_data)
                elif isinstance(request_data, (list, tuple)):
                    return method(*request_data)
                else:
                    return method(request_data)
        else:
            return method()

class MethodStub:
    def __init__(self, stub: 'ServiceStub', method_name: str, is_stream: bool = False, is_stream_iter: bool = False):
        self.stub = stub
        self.method_name = method_name
        self.is_stream = is_stream
        self.is_stream_iter = is_stream_iter

    def __call__(self, *args, **kwargs):
        # Handle parameters - support protobuf auto-serialization
        if len(args) == 0 and len(kwargs) == 0:
            data = None
        elif len(args) == 1 and len(kwargs) == 0:
            data = args[0]
        elif len(args) > 0 and len(kwargs) == 0:
            data = list(args)
        elif len(kwargs) > 0 and len(args) == 0:
            data = kwargs
        else:
            data = kwargs
            for i, arg in enumerate(args):
                data[f'arg{i}'] = arg

        full_method_name = f"{self.stub.service_name}.{self.method_name}"

        # Smart serialization of data
        serialized_data = smart_serialize(data)

        # Get expected return type
        expected_return_type = None
        if hasattr(self.stub.connection_handler, '_method_type_info'):
            method_info = self.stub.connection_handler._method_type_info.get(self.method_name)
            if method_info:
                param_types, return_type = method_info
                expected_return_type = return_type

        if self.is_stream_iter:
            return self.stub.connection_handler._call_stream_iter_method(
                self.stub.peer_id, full_method_name, serialized_data
            )
        elif self.is_stream:
            # Stream call
            future = self.stub.connection_handler._call_stream_method(
                self.stub.peer_id, full_method_name, serialized_data
            )

            return FutureWrapper(future, expected_return_type, is_stream=True)
        else:
            # Regular RPC call
            future = self.stub.connection_handler._call_method(
                self.stub.peer_id, full_method_name, serialized_data
            )
            # Smart deserialization of response using expected return type
            return FutureWrapper(future, expected_return_type, is_stream=False)

class ServiceStub:
    def __init__(self, connection_handler: 'ConnectionHandler', peer_id: str, service_name: str):
        self.connection_handler = connection_handler
        self.peer_id = peer_id
        self.service_name = service_name
        self._method_cache = {}

    def __getattr__(self, name: str):
        if name in self._method_cache:
            return self._method_cache[name]

        is_stream = name in getattr(self.connection_handler, '_stream_methods', [])
        is_stream_iter = name in getattr(self.connection_handler, '_stream_iter_methods', [])

        method_stub = MethodStub(self, name, is_stream, is_stream_iter)
        self._method_cache[name] = method_stub
        return method_stub

class ConnectionHandler(metaclass=ConnectionHandlerMeta):
    def __init__(self, lattica_instance: Lattica):
        self.lattica_instance = lattica_instance
        self._service_name = self.__class__.__name__
        self._register_service()

    def _register_service(self):
        try:
            self.lattica_instance.register_service(self)
        except Exception as e:
            raise

    def get_service_name(self) -> str:
        return self._service_name

    def get_methods(self) -> List[str]:
        return getattr(self, '_rpc_methods', [])

    def get_stream_methods(self) -> List[str]:
        return getattr(self, '_stream_methods', [])

    def get_stub(self, peer_id: str) -> ServiceStub:
        return ServiceStub(self, peer_id, self._service_name)

    def _call_method(self, peer_id: str, method_name: str, data: bytes) -> bytes:
        try:
            client = self.lattica_instance.get_client(peer_id)
            return client.call(method_name, data)
        except Exception as e:
            raise

    def _call_stream_method(self, peer_id: str, method_name: str, data: bytes):
        try:
            client = self.lattica_instance.get_client(peer_id)
            return client.call_stream(method_name, data)
        except Exception as e:
            raise e

    def _call_stream_iter_method(self, peer_id: str, method_name: str, data: bytes):
        try:
            client = self.lattica_instance.get_client(peer_id)
            return client.call_stream_iter(method_name, data)
        except Exception as e:
            raise

class FutureWrapper:
    def __init__(self, future, expected_return_type, is_stream=False):
        self.future = future
        self.expected_return_type = expected_return_type
        self.is_stream = is_stream

    def result(self, timeout=180):
        raw_result = self.future.result(timeout=timeout)
        return self._process_result(raw_result)

    def __await__(self):
        return self._async_result().__await__()

    async def _async_result(self):
        raw_result = await self.future
        return self._process_result(raw_result)

    def _process_result(self, raw_result):
        if self.is_stream:
            return smart_deserialize(raw_result, self.expected_return_type)
        else:
            return smart_deserialize(raw_result, self.expected_return_type)