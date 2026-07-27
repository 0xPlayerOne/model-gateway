"""Keep coverage.py usable with runners that ship malformed sysconfig schemes."""

import sysconfig

_get_paths = sysconfig.get_paths


def get_paths(scheme=None, vars=None, expand=True):
    try:
        return _get_paths(scheme, vars, expand)
    except ValueError as error:
        if "Single '}' encountered in format string" not in str(error):
            raise
        return {}


sysconfig.get_paths = get_paths
