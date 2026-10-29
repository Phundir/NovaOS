"""The parts of Nova's sys module written in Python, set onto it when it
is first imported: the struct sequences describing numbers, flags and
the implementation, and small functions."""

import sys


class _StructSeq(tuple):
    _fields = ()
    _name = ''

    def __new__(cls, values):
        self = tuple.__new__(cls, values)
        return self

    def __getattr__(self, name):
        try:
            return self[type(self)._fields.index(name)]
        except ValueError:
            raise AttributeError(name)

    def __repr__(self):
        return '%s(%s)' % (type(self)._name, ', '.join('%s=%r' % (n, v) for n, v in zip(type(self)._fields, self)))


def _struct(name, fields):
    return type(name.rsplit('.', 1)[-1], (_StructSeq,), {'_fields': tuple(fields.split()), '_name': name,
                                                          '__nova_type_name__': name})


float_info = _struct('sys.float_info', 'max max_exp max_10_exp min min_exp min_10_exp dig mant_dig epsilon radix rounds')(
    (1.7976931348623157e+308, 1024, 308, 2.2250738585072014e-308, -1021, -307, 15, 53, 2.220446049250313e-16, 2, 1))
int_info = _struct('sys.int_info', 'bits_per_digit sizeof_digit default_max_str_digits str_digits_check_threshold')(
    (30, 4, 4300, 640))
hash_info = _struct('sys.hash_info', 'width modulus inf nan imag algorithm hash_bits seed_bits cutoff')(
    (64, 2305843009213693951, 314159, 0, 1000003, 'siphash13', 64, 128, 0))
flags = _struct('sys.flags', 'debug inspect interactive optimize dont_write_bytecode no_user_site no_site '
                'ignore_environment verbose bytes_warning quiet hash_randomization isolated dev_mode '
                'utf8_mode warn_default_encoding safe_path int_max_str_digits gil')(
    (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, False, 1, 0, False, 4300, 1))
thread_info = _struct('sys.thread_info', 'name lock version')(('nova', None, None))


class _Implementation:
    name = 'nova'
    cache_tag = None
    version = sys.version_info
    hexversion = 0x30d00f0
    _multiarch = 'x86_64-nova'

    def __repr__(self):
        return "namespace(name='nova', cache_tag=None, version=%r, hexversion=%d)" % (self.version, self.hexversion)


def getdefaultencoding():
    return 'utf-8'


def getfilesystemencoding():
    return 'utf-8'


def getfilesystemencodeerrors():
    return 'surrogateescape'


def getrefcount(obj):
    return 2


def getswitchinterval():
    return 0.005


def setswitchinterval(interval):
    pass


def is_finalizing():
    return False


def audit(event, *args):
    pass


def addaudithook(hook):
    pass


def _getframe(depth=0):
    raise ValueError('call stack is not deep enough')


def displayhook(value):
    if value is not None:
        import builtins
        builtins._ = None
        print(repr(value))
        builtins._ = value


def excepthook(exctype, value, tb):
    import traceback
    traceback.print_exception(exctype, value, tb)


def unraisablehook(unraisable):
    pass


def activate_stack_trampoline(backend):
    pass


for _name in ('float_info', 'int_info', 'hash_info', 'flags', 'thread_info', 'getdefaultencoding',
              'getfilesystemencoding', 'getfilesystemencodeerrors', 'getrefcount', 'getswitchinterval',
              'setswitchinterval', 'is_finalizing', 'audit', 'addaudithook', '_getframe', 'displayhook',
              'excepthook', 'unraisablehook'):
    setattr(sys, _name, globals()[_name])
sys.implementation = _Implementation()
sys.maxunicode = 1114111
sys.float_repr_style = 'short'
sys.hexversion = 0x30d00f0
sys.api_version = 1013
sys.copyright = 'Copyright (c) 2001 Python Software Foundation.'
sys.dont_write_bytecode = True
sys.prefix = sys.base_prefix = sys.exec_prefix = sys.base_exec_prefix = '/systeme/python'
sys.platlibdir = 'lib'
sys.pycache_prefix = None
sys.abiflags = ''
sys.warnoptions = []
sys._xoptions = {}
sys.orig_argv = list(sys.argv)
sys.meta_path = []
sys.path_hooks = []
sys.path_importer_cache = {}
sys.stdlib_module_names = frozenset(sys.builtin_module_names)
sys.__stdout__ = sys.stdout
sys.__stderr__ = sys.stderr
sys.__stdin__ = sys.stdin
sys.ps1 = '>>> '
sys.ps2 = '... '
