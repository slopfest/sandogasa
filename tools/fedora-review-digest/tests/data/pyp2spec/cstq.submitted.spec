Name:           python-cstq
Version:        0.0.20
Release:        %autorelease
Summary:        Query and modify Python source code using LibCST

License:        MIT
URL:            https://github.com/aleivag/cstq
Source:         %{pypi_source cstq}

BuildArch:      noarch

BuildRequires:  python3-devel
BuildRequires:  python3dist(pytest)

%global _description %{expand:
cstq is a simple library to navigate and manipulate Python source code, based
on LibCST. Nodes of the concrete syntax tree can be selected by attribute
access, filtering and searching, and then changed, replaced, inserted or
removed.}

%description %_description

%package -n     python3-cstq
Summary:        %{summary}

%description -n python3-cstq %_description

%prep
%autosetup -p1 -n cstq-%{version}

%generate_buildrequires
%pyproject_buildrequires

%build
%pyproject_wheel

%install
%pyproject_install
%pyproject_save_files -l cstq

%check
%pyproject_check_import
%pytest -v

%files -n python3-cstq -f %{pyproject_files}
%doc README.md docs/

%changelog
%autochangelog
